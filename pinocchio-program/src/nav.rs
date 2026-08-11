//! Reserve/supply accounting — the wrapped token is a SHARE, not a receipt.
//!
//! Upstream `token-wrap` is strictly 1:1: escrow balance always equals wrapped
//! supply, and the two are interchangeable at par. That makes the wrapper a
//! pure format shim — useful for moving between SPL and Token-2022, and unable
//! to express any notion of the reserves growing.
//!
//! Here the wrapped mint is a claim on a pool instead:
//!
//! ```text
//!   wrap:    shares = assets * supply / reserves
//!   unwrap:  assets = shares * reserves / supply
//! ```
//!
//! where `reserves` is the escrow balance and `supply` is the wrapped mint's
//! supply. Whenever reserves rise without shares being minted — or supply
//! falls without reserves leaving — every remaining share is worth more. That
//! is the entire yield mechanism, and it needs no custom fee logic: Token-2022's
//! `TransferFee` extension withholds on transfer, `harvest_withheld_tokens_to_mint`
//! is permissionless, and burning the harvested balance drops supply directly.
//!
//! ROUNDING IS NOT COSMETIC. Every operation rounds in the direction that
//! favours the pool, never the caller:
//!
//!   - minting shares rounds DOWN, so a depositor can never mint more claim
//!     than they paid for;
//!   - redeeming assets rounds DOWN, so a redeemer can never withdraw more
//!     than their share is worth.
//!
//! Rounding the other way is the classic ERC-4626 drain: repeated deposits and
//! withdrawals each taking one extra unit, funded by everyone else.

use crate::error::WrapError as ProgramError;

#[allow(unused_imports)]
use crate::error::WrapError;

/// Minimum shares burned into the first deposit and never redeemable.
///
/// Defends the empty-vault inflation attack: with zero supply the first
/// depositor sets the price arbitrarily, so an attacker deposits 1 unit,
/// donates a large balance directly to the escrow, and every subsequent
/// depositor's shares round to zero. Permanently locking a small amount makes
/// the ratio impossible to manipulate cheaply. Uniswap V2 uses the same trick
/// for the same reason.
pub const MINIMUM_LIQUIDITY: u64 = 1_000;

/// Shares to mint for `assets` deposited into a pool of (`reserves`, `supply`).
///
/// `reserves` MUST be the escrow balance BEFORE this deposit lands, or the
/// depositor is priced against their own money and mints too few shares.
pub fn shares_for_assets(assets: u64, reserves: u64, supply: u64) -> Result<u64, ProgramError> {
    if assets == 0 {
        return Ok(0);
    }
    // First deposit: fix the ratio at 1:1 and lock MINIMUM_LIQUIDITY forever.
    // Seeding at anything else would let the initialiser pick the price.
    if supply == 0 || reserves == 0 {
        return assets
            .checked_sub(MINIMUM_LIQUIDITY)
            .ok_or(ProgramError::InsufficientFunds);
    }
    // u128 throughout: assets * supply overflows u64 for any realistic pool
    // (1e12 units at 1e12 supply is already 1e24).
    let n = (assets as u128)
        .checked_mul(supply as u128)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    let q = n / (reserves as u128); // floor — favours the pool
    u64::try_from(q).map_err(|_| ProgramError::ArithmeticOverflow)
}

/// Assets returned for burning `shares` from a pool of (`reserves`, `supply`).
pub fn assets_for_shares(shares: u64, reserves: u64, supply: u64) -> Result<u64, ProgramError> {
    if shares == 0 {
        return Ok(0);
    }
    if supply == 0 {
        return Err(ProgramError::InsufficientFunds);
    }
    let n = (shares as u128)
        .checked_mul(reserves as u128)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    let q = n / (supply as u128); // floor — favours the pool
    u64::try_from(q).map_err(|_| ProgramError::ArithmeticOverflow)
}

/// Value of one whole share, scaled by 10^decimals. View helper for clients.
pub fn nav_per_share(reserves: u64, supply: u64, decimals: u8) -> Result<u64, ProgramError> {
    if supply == 0 {
        return Ok(0);
    }
    let one = 10u128
        .checked_pow(decimals as u32)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    let n = (reserves as u128)
        .checked_mul(one)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    u64::try_from(n / (supply as u128)).map_err(|_| ProgramError::ArithmeticOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_deposit_locks_minimum_liquidity() {
        // 1:1 minus the permanently locked floor.
        assert_eq!(shares_for_assets(10_000, 0, 0).unwrap(), 10_000 - MINIMUM_LIQUIDITY);
        // A first deposit too small to cover the lock must fail, not underflow.
        assert!(shares_for_assets(MINIMUM_LIQUIDITY - 1, 0, 0).is_err());
    }

    #[test]
    fn nav_rises_when_reserves_grow_without_minting() {
        // Pool: 1000 reserves / 1000 supply. Someone donates 100 to escrow.
        // A later depositor of 100 must get FEWER than 100 shares.
        let s = shares_for_assets(100, 1100, 1000).unwrap();
        assert!(s < 100, "expected < 100 shares, got {s}");
        assert_eq!(s, 90); // 100 * 1000 / 1100
    }

    #[test]
    fn nav_rises_when_supply_burns_without_reserves_leaving() {
        // This is the TransferFee path: fees harvest to the mint and burn, so
        // supply drops while reserves stay put.
        let before = nav_per_share(1000, 1000, 6).unwrap();
        let after = nav_per_share(1000, 900, 6).unwrap();
        assert!(after > before, "burning supply must raise NAV");
    }

    #[test]
    fn round_trip_never_profits_the_caller() {
        // The invariant that matters: deposit then immediately withdraw must
        // never return more than was put in. If it can, the pool is drainable
        // one unit at a time.
        for assets in [1u64, 7, 999, 1_000_000, u32::MAX as u64] {
            let (r, s) = (5_000_000u64, 4_000_000u64);
            let shares = shares_for_assets(assets, r, s).unwrap();
            let back = assets_for_shares(shares, r + assets, s + shares).unwrap();
            assert!(back <= assets, "drain: put {assets} got {back}");
        }
    }

    #[test]
    fn empty_pool_cannot_be_redeemed() {
        assert!(assets_for_shares(1, 0, 0).is_err());
    }

    #[test]
    fn large_values_do_not_overflow() {
        let big = u64::MAX / 2;
        assert!(shares_for_assets(big, big, big).is_ok());
        assert!(assets_for_shares(big, big, big).is_ok());
    }
}
