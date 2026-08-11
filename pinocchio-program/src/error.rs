//! Errors, as plain discriminants.
//!
//! Pinocchio deliberately has no `ProgramError` enum with a hundred variants —
//! a program returns a `u64`. Defining our own keeps the binary small and the
//! meanings ours, instead of inheriting a taxonomy written for a different
//! runtime.

use pinocchio::error::ProgramError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum WrapError {
    /// Deposit priced to zero shares at the current NAV.
    DepositTooSmall = 100,
    /// Redemption priced to zero assets at the current NAV.
    RedeemTooSmall = 101,
    /// Arithmetic would overflow — never silently wrap a balance.
    ArithmeticOverflow = 102,
    /// First deposit below `MINIMUM_LIQUIDITY`.
    InsufficientFunds = 103,
    /// Wrapped mint authority PDA did not match the derivation.
    MintAuthorityMismatch = 104,
    /// Mint carries no TransferFee extension, so there is nothing to crank.
    NoTransferFeeExtension = 105,
    /// Account list shorter than the instruction requires.
    NotEnoughAccounts = 106,
    /// Unknown instruction discriminant.
    InvalidInstruction = 107,
    /// Backpointer account is not owned by this program — its bytes are
    /// whatever an unrelated program wrote, so they must not be trusted.
    InvalidBackpointerOwner = 108,
    /// The escrow handed to `init_backpointer` is not the authority-owned
    /// reserves account for the mint pair being registered. Without this check
    /// anyone could register an arbitrary mapping.
    BackpointerEscrowMismatch = 109,
    /// Backpointer already registered a DIFFERENT pair. Re-registering would
    /// silently repoint an existing market.
    BackpointerConflict = 110,
}

impl From<WrapError> for ProgramError {
    fn from(e: WrapError) -> Self {
        ProgramError::Custom(e as u32)
    }
}
