//! OxDEX on-chain settlement program (scaffold).
//!
//! This program is intentionally a **skeleton** in this revision. The full
//! settlement logic is described in the workspace README and in
//! `programs/oxdex-settlement/README.md`.
//!
//! ## Account layout (planned)
//!
//! * `settlement_pda` — singleton, holds program config and the per-user
//!   nonce-bitmap PDAs.
//! * `solver_registry` — PDA mapping `solver_pubkey → stake / status`.
//! * `nonce_bitmap[user]` — 256-bit (or larger) bitmap PDA per user, used
//!   to enforce single-use nonces cheaply.
//!
//! ## Instructions (planned)
//!
//! * `init_config(admin)` — one-time setup.
//! * `register_solver(stake)` — solver opt-in with SOL stake.
//! * `settle(batch_id, clearing_prices, trades, interactions)` — atomic
//!   settlement. Verifies Ed25519 sigs (via the SigVerify precompile in the
//!   same tx), enforces uniform clearing prices, performs delegate-based
//!   SPL transfers, optionally CPI-calls whitelisted DEX programs for
//!   residual liquidity, and asserts a global conservation invariant.
//! * `cancel_order(order_id)` — owner-signed on-chain cancel that flips the
//!   nonce bit so off-chain auctioneers can no longer settle the order.

use anchor_lang::prelude::*;

declare_id!("oxDEX1111111111111111111111111111111111111");

#[program]
pub mod oxdex_settlement {
    use super::*;

    /// One-time admin configuration.
    pub fn init_config(_ctx: Context<InitConfig>) -> Result<()> {
        // TODO(settlement): persist admin pubkey + protocol params.
        Ok(())
    }

    /// Solver opts in with a SOL stake.
    pub fn register_solver(_ctx: Context<RegisterSolver>, _stake_lamports: u64) -> Result<()> {
        // TODO(settlement): collect SOL into the program PDA, mark solver active.
        Ok(())
    }

    /// Atomically settle a batch.
    pub fn settle(_ctx: Context<Settle>, _batch_id: u64) -> Result<()> {
        // TODO(settlement):
        //   1. Verify each Order via the SigVerify precompile output.
        //   2. Check valid_to and nonce-bitmap not used; flip bit.
        //   3. Enforce uniform clearing prices.
        //   4. CPI Token::transfer for each trade leg via delegate.
        //   5. Optional CPI for residual AMM legs (whitelisted programs).
        //   6. Assert per-mint conservation invariant on the program PDA.
        Ok(())
    }
}

#[derive(Accounts)]
pub struct InitConfig<'info> {
    /// The admin paying for the config account.
    #[account(mut)]
    pub admin: Signer<'info>,
    /// Required for account creation.
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct RegisterSolver<'info> {
    /// Solver opting in.
    #[account(mut)]
    pub solver: Signer<'info>,
    /// Required for account creation.
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Settle<'info> {
    /// Whitelisted solver submitting the settlement.
    pub solver: Signer<'info>,
}

