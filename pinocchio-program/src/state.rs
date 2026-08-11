//! Zero-copy reads of SPL Token / Token-2022 account data.
//!
//! Both programs lay the base `Mint` and `Account` structs out identically at
//! the start of the buffer; Token-2022 appends a TLV extension region after a
//! 83-byte padding marker. So the fields NAV needs — mint supply, mint
//! decimals, token account amount — are at fixed offsets in both, and can be
//! read without unpacking anything.
//!
//! Upstream reaches for `PodStateWithExtensions`, which pulls in the whole
//! spl-token-2022 crate. Reading four fields at known offsets avoids that
//! entirely, which is most of why this build is small.

use crate::error::WrapError;
use pinocchio::{account::AccountView, error::ProgramError};

/// Token account: `amount` is a u64 at offset 64 (after mint + owner pubkeys).
pub const ACCOUNT_AMOUNT_OFFSET: usize = 64;
/// Mint: `supply` is a u64 at offset 36 (after COption<Address> mint_authority).
pub const MINT_SUPPLY_OFFSET: usize = 36;
/// Mint: `decimals` is a u8 immediately after supply.
pub const MINT_DECIMALS_OFFSET: usize = 44;

fn read_u64_at(data: &[u8], off: usize) -> Result<u64, ProgramError> {
    let bytes: [u8; 8] = data
        .get(off..off + 8)
        .and_then(|s| s.try_into().ok())
        .ok_or(ProgramError::from(WrapError::NotEnoughAccounts))?;
    Ok(u64::from_le_bytes(bytes))
}

/// Balance of a token account, read in place.
pub fn token_account_amount(account: &AccountView) -> Result<u64, ProgramError> {
    let data = account.try_borrow()?;
    read_u64_at(&data, ACCOUNT_AMOUNT_OFFSET)
}

/// Supply of a mint, read in place.
pub fn mint_supply(account: &AccountView) -> Result<u64, ProgramError> {
    let data = account.try_borrow()?;
    read_u64_at(&data, MINT_SUPPLY_OFFSET)
}

/// Decimals of a mint, read in place.
pub fn mint_decimals(account: &AccountView) -> Result<u8, ProgramError> {
    let data = account.try_borrow()?;
    data.get(MINT_DECIMALS_OFFSET)
        .copied()
        .ok_or(ProgramError::from(WrapError::NotEnoughAccounts))
}
