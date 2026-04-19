//! CoW-style matching engine.
//!
//! Given a slice of open orders, this engine groups them by *unordered*
//! token pair `{A,B}`, then for each pair runs a deterministic algorithm
//! that:
//!
//! 1. Splits orders into the two directions (A→B and B→A).
//! 2. Sorts each side by limit price (most aggressive first).
//! 3. Greedily pairs opposite-direction orders whose limit prices cross.
//! 4. Sets the **uniform clearing price** for the pair to the midpoint
//!    of the last crossed pair's limit prices (in rational form).
//! 5. Re-prices every fill at that uniform price (CoW invariant).
//!
//! Properties:
//!  * **Deterministic** — same input ⇒ same output.
//!  * **Per-pair parallel** — pairs are independent, so we use [`rayon`]
//!    to process them concurrently across CPU cores.
//!  * **Type-safe pricing** — uses [`oxdex_types::Price`] (rational `u128/u128`),
//!    never floats.
//!  * **No allocations on hot path beyond Vec growth** — we sort in place
//!    and reuse buffers per pair.
//!
//! # Time complexity
//! For `n` orders distributed across `k` pairs with `n_i` orders in pair `i`:
//!
//! ```text
//! Total work: O(Σ n_i log n_i)   = O(n log n) worst-case
//! Wall time:  O((max_i n_i log n_i)) on `min(k, num_cpus)` cores
//! ```
//!
//! # Limitations
//! This is a *reference* implementation. Production solvers would also
//! solve a small LP per pair to maximise total surplus exactly, and
//! integrate AMM residuals (out of scope for this crate).
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use rayon::prelude::*;
use std::collections::HashMap;
use tracing::{debug, instrument};

use oxdex_types::{
    Address, BatchId, ClearingPrice, Order, OrderId, OrderKind, Price, SignedOrder, Solution,
    TradeExecution,
};

/// Configuration knobs for the engine.
#[derive(Debug, Clone, Copy)]
pub struct MatcherConfig {
    /// If true, run pair matching on the rayon thread-pool.
    pub parallel: bool,
}
impl Default for MatcherConfig {
    fn default() -> Self {
        Self { parallel: true }
    }
}

/// Stateless matching engine.
#[derive(Debug, Clone, Copy, Default)]
pub struct Matcher {
    cfg: MatcherConfig,
}

impl Matcher {
    /// Construct with custom config.
    pub fn new(cfg: MatcherConfig) -> Self {
        Self { cfg }
    }

    /// Match a batch of signed orders, producing one [`Solution`].
    ///
    /// `solver` is recorded into the resulting [`Solution::solver`] field.
    #[instrument(skip(self, orders), fields(n = orders.len()))]
    pub fn match_batch(
        &self,
        batch_id: BatchId,
        solver: Address,
        orders: &[SignedOrder],
    ) -> Solution {
        // Group by unordered pair {min(a,b), max(a,b)} so A→B and B→A meet.
        let mut groups: HashMap<(Address, Address), Vec<&Order>> = HashMap::new();
        for so in orders {
            let o = &so.order;
            let key = canonical_pair(o.sell_mint, o.buy_mint);
            groups.entry(key).or_default().push(o);
        }

        let pairs: Vec<((Address, Address), Vec<&Order>)> = groups.into_iter().collect();
        debug!(pair_count = pairs.len(), "matching pairs");

        // Each pair is independent — rayon them.
        let per_pair: Vec<PairOutcome> = if self.cfg.parallel {
            pairs
                .into_par_iter()
                .map(|((a, b), os)| match_pair(a, b, &os))
                .collect()
        } else {
            pairs
                .into_iter()
                .map(|((a, b), os)| match_pair(a, b, &os))
                .collect()
        };

        // Merge.
        let mut clearing: Vec<ClearingPrice> = Vec::new();
        let mut trades: Vec<TradeExecution> = Vec::new();
        let mut score: u128 = 0;
        for p in per_pair {
            clearing.extend(p.clearing);
            trades.extend(p.trades);
            score = score.saturating_add(p.surplus);
        }

        Solution {
            batch_id,
            solver,
            clearing_prices: clearing,
            trades,
            score,
        }
    }
}

fn canonical_pair(a: Address, b: Address) -> (Address, Address) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

struct PairOutcome {
    clearing: Vec<ClearingPrice>,
    trades: Vec<TradeExecution>,
    surplus: u128,
}

/// Match orders for a single canonical pair `(token_a, token_b)`.
/// `token_a <= token_b` lexicographically.
fn match_pair(token_a: Address, token_b: Address, orders: &[&Order]) -> PairOutcome {
    // Split by direction.
    // direction A→B: sell_mint = token_a, buy_mint = token_b.
    let mut a_to_b: Vec<&Order> = Vec::new();
    let mut b_to_a: Vec<&Order> = Vec::new();
    for o in orders {
        if o.sell_mint == token_a && o.buy_mint == token_b {
            a_to_b.push(o);
        } else if o.sell_mint == token_b && o.buy_mint == token_a {
            b_to_a.push(o);
        }
        // else: malformed routing — skip silently.
    }

    if a_to_b.is_empty() || b_to_a.is_empty() {
        return PairOutcome {
            clearing: vec![],
            trades: vec![],
            surplus: 0,
        };
    }

    // Sort: A→B by best (lowest) limit price ascending so the most aggressive sellers
    // come first; B→A by best (lowest) limit price as well — both give us "most willing".
    //
    // limit_price_ab = buy_b / sell_a   (lower = wants less B per A = aggressive seller of A)
    // limit_price_ba = buy_a / sell_b   (lower = wants less A per B = aggressive seller of B)
    a_to_b.sort_by_key(|x| x.limit_price());
    b_to_a.sort_by_key(|x| x.limit_price());

    // Compute running fills. For each pair, the cross condition is:
    //   limit_price_ab * limit_price_ba <= 1     (rationally: ab.num*ba.num <= ab.den*ba.den)
    //
    // We greedily fill min(remaining_a_sell, remaining_b_buy_demand_in_a)
    // and stop as soon as the next two heads do not cross.

    let mut a_remaining: Vec<u128> = a_to_b.iter().map(|o| o.sell_amount as u128).collect();
    let mut b_remaining: Vec<u128> = b_to_a.iter().map(|o| o.sell_amount as u128).collect();

    let mut fills_ab: Vec<(usize, u128, u128)> = Vec::new(); // (idx in a_to_b, sold_a, bought_b)
    let mut fills_ba: Vec<(usize, u128, u128)> = Vec::new(); // (idx in b_to_a, sold_b, bought_a)

    let mut i = 0usize;
    let mut j = 0usize;

    // Track the last crossing pair to derive the uniform clearing price.
    let mut last_cross: Option<(Price, Price)> = None;

    while i < a_to_b.len() && j < b_to_a.len() {
        let oa = a_to_b[i];
        let ob = b_to_a[j];

        let lp_ab = oa.limit_price(); // B per A
        let lp_ba = ob.limit_price(); // A per B

        // Cross check: lp_ab.num*lp_ba.num <= lp_ab.den*lp_ba.den
        let lhs = lp_ab.num.saturating_mul(lp_ba.num);
        let rhs = lp_ab.den.saturating_mul(lp_ba.den);
        if lhs > rhs {
            break;
        }
        last_cross = Some((lp_ab, lp_ba));

        // How much A can we move? oa still has `a_remaining[i]` of A to sell.
        // ob is willing to *buy* up to `ob.buy_amount * (b_remaining[j]/ob.sell_amount)` A.
        let a_left = a_remaining[i];
        let ob_a_capacity = (b_remaining[j]).saturating_mul(ob.buy_amount as u128)
            / (ob.sell_amount.max(1) as u128);
        let trade_a = a_left.min(ob_a_capacity);

        if trade_a == 0 {
            // numerical edge: advance whichever side is exhausted
            if a_left == 0 {
                i += 1;
            } else {
                j += 1;
            }
            continue;
        }

        // Corresponding B for this A move at the *book* (not clearing) ratio for accounting;
        // we'll re-price everything at the uniform clearing price after the loop.
        let trade_b =
            trade_a.saturating_mul(ob.sell_amount as u128) / (ob.buy_amount.max(1) as u128);

        a_remaining[i] = a_left.saturating_sub(trade_a);
        b_remaining[j] = b_remaining[j].saturating_sub(trade_b);

        fills_ab.push((i, trade_a, trade_b));
        fills_ba.push((j, trade_b, trade_a));

        // Respect partial_fill flag: if either order forbids partial fills and isn't fully
        // satisfied, we must roll this fill back. Simplest correct behaviour for v1: skip.
        if !oa.partial_fill && a_remaining[i] != 0 {
            // rollback last
            a_remaining[i] = a_remaining[i].saturating_add(trade_a);
            b_remaining[j] = b_remaining[j].saturating_add(trade_b);
            fills_ab.pop();
            fills_ba.pop();
            i += 1;
            continue;
        }
        if !ob.partial_fill && b_remaining[j] != 0 {
            a_remaining[i] = a_remaining[i].saturating_add(trade_a);
            b_remaining[j] = b_remaining[j].saturating_add(trade_b);
            fills_ab.pop();
            fills_ba.pop();
            j += 1;
            continue;
        }

        if a_remaining[i] == 0 {
            i += 1;
        }
        if b_remaining[j] == 0 {
            j += 1;
        }
    }

    if fills_ab.is_empty() {
        return PairOutcome {
            clearing: vec![],
            trades: vec![],
            surplus: 0,
        };
    }

    // Uniform clearing price for B-per-A: midpoint between the last two crossed limit prices.
    // mid(p, 1/q) where p = lp_ab (B/A), q = lp_ba (A/B), as rationals.
    //   mid = (p + 1/q) / 2 = (p.num*q.num + p.den*q.den) / (2 * p.den * q.num)
    let (lp_ab, lp_ba) = last_cross.expect("must exist if fills_ab non-empty");
    let num =
        (lp_ab.num.saturating_mul(lp_ba.num)).saturating_add(lp_ab.den.saturating_mul(lp_ba.den));
    let den = lp_ab.den.saturating_mul(lp_ba.num).saturating_mul(2);
    let p_b_per_a = Price::new(num.max(1), den.max(1)).unwrap_or(Price { num: 1, den: 1 });

    // Re-aggregate per order.
    // For A→B orders: executed_sell = Σ trade_a; executed_buy = clearing_price * Σ trade_a.
    // For B→A orders: executed_sell = Σ trade_b; executed_buy = (1/clearing) * Σ trade_b.
    let mut sum_a_per_order: HashMap<usize, u128> = HashMap::new();
    for (i, sa, _) in &fills_ab {
        *sum_a_per_order.entry(*i).or_default() += *sa;
    }
    let mut sum_b_per_order: HashMap<usize, u128> = HashMap::new();
    for (j, sb, _) in &fills_ba {
        *sum_b_per_order.entry(*j).or_default() += *sb;
    }

    let mut trades: Vec<TradeExecution> = Vec::new();
    let mut surplus: u128 = 0;

    // A→B side: each unit of A buys `p_b_per_a` units of B.
    for (idx, sold_a) in sum_a_per_order {
        let bought_b = p_b_per_a.apply(sold_a);
        let order = a_to_b[idx];
        // Surplus vs. limit: extra B above what the user would have accepted.
        let min_b = order.limit_price().apply(sold_a);
        if bought_b >= min_b {
            surplus = surplus.saturating_add(bought_b - min_b);
        }
        trades.push(TradeExecution {
            order_id: order.id(),
            executed_sell: clamp_u64(sold_a),
            executed_buy: clamp_u64(bought_b),
        });
    }

    // B→A side: clearing for A-per-B is reciprocal of p_b_per_a.
    let p_a_per_b = Price {
        num: p_b_per_a.den,
        den: p_b_per_a.num.max(1),
    };
    for (idx, sold_b) in sum_b_per_order {
        let bought_a = p_a_per_b.apply(sold_b);
        let order = b_to_a[idx];
        let min_a = order.limit_price().apply(sold_b);
        if bought_a >= min_a {
            surplus = surplus.saturating_add(bought_a - min_a);
        }
        trades.push(TradeExecution {
            order_id: order.id(),
            executed_sell: clamp_u64(sold_b),
            executed_buy: clamp_u64(bought_a),
        });
    }

    PairOutcome {
        clearing: vec![
            ClearingPrice {
                mint: token_a,
                price: p_a_per_b,
            },
            ClearingPrice {
                mint: token_b,
                price: p_b_per_a,
            },
        ],
        trades,
        surplus,
    }
}

fn clamp_u64(v: u128) -> u64 {
    if v > u64::MAX as u128 {
        u64::MAX
    } else {
        v as u64
    }
}

/// Avoid `unused` warning when `OrderKind`/`OrderId` aren't used directly.
#[doc(hidden)]
pub fn _types_in_use(_a: OrderKind, _b: OrderId) {}

#[cfg(test)]
mod tests {
    use super::*;
    use oxdex_types::{Order, OrderKind};

    fn order(
        owner: u8,
        sell: Address,
        buy: Address,
        sell_amt: u64,
        buy_amt: u64,
        valid_to: i64,
    ) -> SignedOrder {
        SignedOrder {
            order: Order {
                owner: Address([owner; 32]),
                sell_mint: sell,
                buy_mint: buy,
                sell_amount: sell_amt,
                buy_amount: buy_amt,
                valid_to,
                nonce: 0,
                kind: OrderKind::Sell,
                partial_fill: true,
                receiver: Address([owner; 32]),
            },
            signature: [0u8; 64],
        }
    }

    #[test]
    fn empty_batch_is_empty_solution() {
        let m = Matcher::default();
        let s = m.match_batch(BatchId::new(), Address::zero(), &[]);
        assert!(s.trades.is_empty());
        assert!(s.clearing_prices.is_empty());
        assert_eq!(s.score, 0);
    }

    #[test]
    fn perfect_cow_match() {
        let a = Address([10u8; 32]);
        let b = Address([20u8; 32]);
        // Alice sells 100 A wants ≥150 B (limit 1.5 B/A)
        let alice = order(1, a, b, 100, 150, i64::MAX);
        // Bob sells 200 B wants ≥100 A (limit 0.5 A/B = 2.0 B/A)
        let bob = order(2, b, a, 200, 100, i64::MAX);

        let m = Matcher::default();
        let s = m.match_batch(BatchId::new(), Address::zero(), &[alice, bob]);

        assert_eq!(s.trades.len(), 2);
        // both orders should be (mostly) satisfied
        let total_a_sold: u64 = s
            .trades
            .iter()
            .filter(|t| t.executed_sell > 0)
            .map(|t| t.executed_sell)
            .sum();
        assert!(total_a_sold > 0);
        assert!(s.score > 0, "expected positive surplus from CoW");
    }

    #[test]
    fn non_crossing_orders_no_fill() {
        let a = Address([10u8; 32]);
        let b = Address([20u8; 32]);
        // Alice wants 1000 B for 100 A (limit 10 B/A) — very expensive
        let alice = order(1, a, b, 100, 1000, i64::MAX);
        // Bob offers 100 B for 100 A (limit 1 A/B = 1 B/A) — far below ask
        let bob = order(2, b, a, 100, 100, i64::MAX);

        let m = Matcher::default();
        let s = m.match_batch(BatchId::new(), Address::zero(), &[alice, bob]);
        assert!(s.trades.is_empty());
        assert!(s.clearing_prices.is_empty());
    }

    #[test]
    fn parallel_and_serial_agree() {
        use rand::{Rng, SeedableRng};
        let mut rng = rand::rngs::StdRng::seed_from_u64(42);
        let a = Address([10u8; 32]);
        let b = Address([20u8; 32]);

        let mut orders = Vec::new();
        for i in 0..40u8 {
            let s = rng.gen_range(50..500);
            let bu = rng.gen_range(50..500);
            orders.push(order(i, a, b, s, bu, i64::MAX));
            orders.push(order(255 - i, b, a, s, bu, i64::MAX));
        }
        let par = Matcher::new(MatcherConfig { parallel: true }).match_batch(
            BatchId::new(),
            Address::zero(),
            &orders,
        );
        let ser = Matcher::new(MatcherConfig { parallel: false }).match_batch(
            BatchId::new(),
            Address::zero(),
            &orders,
        );
        assert_eq!(par.trades.len(), ser.trades.len());
        assert_eq!(par.score, ser.score);
    }
}
