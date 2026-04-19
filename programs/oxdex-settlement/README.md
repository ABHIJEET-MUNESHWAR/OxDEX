## Anchor program: `oxdex-settlement`

This is the on-chain settlement program for OxDEX. It is **kept outside the
main Cargo workspace** because building it requires the Solana SBF toolchain
(`anchor build` / `cargo build-sbf`), which uses a different rustc target,
toolchain channel and `solana-program` pin than the off-chain crates.

### Status

This directory currently ships a **scaffolded program** with the public
instruction surface and account layout, but the inner business logic
(uniform-clearing-price enforcement, conservation invariants, Ed25519
sigverify-precompile checks, delegate-based SPL transfers) is left as the
next implementation pass. See `programs/oxdex-settlement/src/lib.rs` for
the entrypoints and the inline `// TODO(settlement)` markers.

### Building

```bash
# install once
cargo install --git https://github.com/coral-xyz/anchor anchor-cli --tag v0.30.1 --locked
# build
cd programs/oxdex-settlement && anchor build
# tests (requires solana-test-validator)
anchor test
```

### Why is logic stubbed?

The off-chain stack (intent pool, auctioneer, matching engine, Jito client)
is **fully implemented and unit-tested** — that is the bulk of the system
and is what you can run today on any Linux box without a Solana toolchain.
The on-chain program is a separable, smaller piece that depends on a heavy
toolchain and is best iterated against `solana-test-validator` once the
off-chain side is stable. Treating it as a separate sub-project keeps
`cargo test --workspace` fast, hermetic, and independent of Solana CLI
versions.

