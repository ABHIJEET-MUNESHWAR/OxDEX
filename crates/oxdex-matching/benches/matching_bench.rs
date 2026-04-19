//! Criterion benchmarks for the matching engine.
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use oxdex_matching::{Matcher, MatcherConfig};
use oxdex_types::{Address, BatchId, Order, OrderKind, SignedOrder};
use rand::{Rng, SeedableRng};

fn make_orders(n: usize, pairs: usize) -> Vec<SignedOrder> {
    let mut rng = rand::rngs::StdRng::seed_from_u64(0xDEAD_BEEF);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let pair = i % pairs;
        let a = Address([(pair as u8) * 2 + 1; 32]);
        let b = Address([(pair as u8) * 2 + 2; 32]);
        let (sell, buy) = if i % 2 == 0 { (a, b) } else { (b, a) };
        out.push(SignedOrder {
            order: Order {
                owner: Address([(i % 251) as u8; 32]),
                sell_mint: sell,
                buy_mint: buy,
                sell_amount: rng.gen_range(100..10_000),
                buy_amount: rng.gen_range(100..10_000),
                valid_to: i64::MAX,
                nonce: i as u64,
                kind: OrderKind::Sell,
                partial_fill: true,
                receiver: Address([(i % 251) as u8; 32]),
            },
            signature: [0u8; 64],
        });
    }
    out
}

fn bench_matching(c: &mut Criterion) {
    let mut g = c.benchmark_group("matching");
    for &n in &[100usize, 1_000, 10_000] {
        let orders = make_orders(n, 8);
        g.throughput(Throughput::Elements(n as u64));

        g.bench_with_input(BenchmarkId::new("serial", n), &orders, |b, o| {
            let m = Matcher::new(MatcherConfig { parallel: false });
            b.iter(|| m.match_batch(BatchId::new(), Address::zero(), o))
        });
        g.bench_with_input(BenchmarkId::new("parallel", n), &orders, |b, o| {
            let m = Matcher::new(MatcherConfig { parallel: true });
            b.iter(|| m.match_batch(BatchId::new(), Address::zero(), o))
        });
    }
    g.finish();
}

criterion_group!(benches, bench_matching);
criterion_main!(benches);
