//! Yield-bearing token wrap — Pinocchio implementation.
//!
//! A deliberate rewrite of the core path rather than a mechanical translation
//! of upstream's 5,395 lines. Most of that surface is Metaplex/metadata
//! syncing, which a settlement asset does not need and which would have to be
//! ported CPI-by-CPI for no benefit. What matters is: create the mint, wrap at
//! NAV, unwrap at NAV, crank fees. Four instructions.
//!
//! WHY PINOCCHIO
//! -------------
//! `solana-program`'s entrypoint deserialises every account into an owned
//! `AccountView` on each invocation, before the program has decided which
//! accounts it even needs. Pinocchio reads the input buffer in place, so the
//! per-invocation cost scales with what you touch rather than what you were
//! handed. For an instruction called once per payment on a micropayment rail,
//! that overhead is the product.
//!
//! It is also `no_std` with zero external dependencies, so the emitted `.so`
//! is a fraction of the size — which matters because rent on the program
//! account is paid on bytes.
//!
//! NOTHING ABOUT THE ECONOMICS CHANGES. `nav.rs` is byte-identical to the
//! solana-program build apart from the error type: the same floor-toward-the-
//! pool rounding, the same MINIMUM_LIQUIDITY lock, the same drain test. Two
//! implementations of the same maths is exactly how the two drift apart, so
//! the file is copied rather than reimplemented.

#![no_std]

pub mod error;
pub mod nav;
pub mod processor;
pub mod state;

use pinocchio::{
    account::AccountView, address::Address, entrypoint,
    error::{ProgramError, ProgramResult},
};

// `entrypoint!` bundles `default_allocator!` + `default_panic_handler!`, and
// the default panic handler formats a message — which needs `std`. Under
// `#![no_std]` that fails to link, so the three pieces are declared
// separately with the nostd variants.
//
// `no_allocator!` is deliberate rather than incidental: nothing in this
// program heap-allocates. Every buffer is a fixed-size array on the stack and
// every account read is in place. Declaring it makes that a compile error if
// someone later reaches for a Vec, instead of quietly linking an allocator
// back in and paying for it on every invocation.
pinocchio::program_entrypoint!(process_instruction);
pinocchio::no_allocator!();
pinocchio::nostd_panic_handler!();

/// Instruction discriminants. A single leading byte — no Borsh, no serde.
/// Deserialising a tag with a framework costs more compute than the branch.
#[repr(u8)]
pub enum Instruction {
    /// Create the wrapped mint + escrow for an unwrapped mint.
    CreateMint = 0,
    /// Deposit unwrapped tokens, mint shares at NAV.
    Wrap = 1,
    /// Burn shares, release unwrapped tokens at NAV.
    Unwrap = 2,
    /// Harvest withheld transfer fees, split 50/50 burn/treasury.
    CrankFees = 3,
}

impl TryFrom<u8> for Instruction {
    type Error = ProgramError;
    fn try_from(v: u8) -> Result<Self, ProgramError> {
        match v {
            0 => Ok(Self::CreateMint),
            1 => Ok(Self::Wrap),
            2 => Ok(Self::Unwrap),
            3 => Ok(Self::CrankFees),
            _ => Err(error::WrapError::InvalidInstruction.into()),
        }
    }
}

pub fn process_instruction(
    program_id: &Address,
    accounts: &mut [AccountView],
    data: &[u8],
) -> ProgramResult {
    let (tag, rest) = data
        .split_first()
        .ok_or(ProgramError::from(error::WrapError::InvalidInstruction))?;

    match Instruction::try_from(*tag)? {
        Instruction::CreateMint => processor::create_mint(program_id, accounts),
        // amount (u64 LE) then bump (u8) — see processor for why the bump is
        // passed rather than derived on-chain.
        Instruction::Wrap => processor::wrap(program_id, accounts, read_u64(rest)?, read_bump(rest)?),
        Instruction::Unwrap => {
            processor::unwrap(program_id, accounts, read_u64(rest)?, read_bump(rest)?)
        }
        Instruction::CrankFees => processor::crank_fees(program_id, accounts, read_bump_at(rest, 0)?),
    }
}

/// Bump immediately after the u64 amount.
fn read_bump(data: &[u8]) -> Result<u8, ProgramError> {
    read_bump_at(data, 8)
}

fn read_bump_at(data: &[u8], off: usize) -> Result<u8, ProgramError> {
    data.get(off)
        .copied()
        .ok_or(ProgramError::from(error::WrapError::InvalidInstruction))
}

/// Read a little-endian u64 without a deserialiser.
fn read_u64(data: &[u8]) -> Result<u64, ProgramError> {
    let bytes: [u8; 8] = data
        .get(..8)
        .and_then(|s| s.try_into().ok())
        .ok_or(ProgramError::from(error::WrapError::InvalidInstruction))?;
    Ok(u64::from_le_bytes(bytes))
}
