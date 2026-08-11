//! On-chain registry: wrapped mint → everything needed to use it.
//!
//! WHY THIS EXISTS. The wrapped mint is a caller-generated Keypair, not a PDA
//! of the underlying, so given an unwrapped mint you cannot derive its wrapper.
//! Without a record on-chain there is nothing to enumerate and nothing to
//! resolve — a client has to replay transaction history to learn that a market
//! exists at all. Upstream `token-wrap` solved this with a `Backpointer` PDA;
//! the first cut of this rewrite kept the `CreateMint` instruction name and
//! dropped the account, which quietly removed the registry.
//!
//! WIRE COMPATIBILITY. The seed (`b"backpointer"`) and the first 32 bytes
//! (`unwrapped_mint`) are byte-identical to upstream, so anything that already
//! derives or reads an upstream backpointer keeps working unchanged. The extra
//! fields are appended AFTER that, never interleaved.
//!
//! WHAT THE EXTRA FIELDS BUY. The account costs rent whether it holds 32 bytes
//! or 136, and each of these is something a client would otherwise need an
//! extra RPC round trip (or a 1500-CU `find_program_address`) to obtain:
//!
//! ```text
//!    0..32   unwrapped_mint            upstream-compatible
//!   32..64   escrow                    the reserves account — NAV numerator
//!   64..96   unwrapped_token_program   SPL vs Token-2022; no ix is buildable without it
//!   96..128  wrapped_token_program     the two may differ, that is the point of wrapping
//!      128   authority_bump            so nobody re-derives the PDA at 1500 CU a go
//!      129   decimals                  of the wrapped mint
//!  130..136  reserved                  zeroed; additive room without a realloc
//! ```
//!
//! Enumeration is then one `getProgramAccounts` filtered on `dataSize = 136`.

use {
    crate::error::WrapError,
    pinocchio::{account::AccountView, address::Address, error::ProgramError},
};

/// Byte-identical to upstream's `WRAPPED_MINT_BACKPOINTER_SEED`.
pub const BACKPOINTER_SEED: &[u8] = b"backpointer";

pub const LEN: usize = 136;

const OFF_UNWRAPPED_MINT: usize = 0;
const OFF_ESCROW: usize = 32;
const OFF_UNWRAPPED_TOKEN_PROGRAM: usize = 64;
const OFF_WRAPPED_TOKEN_PROGRAM: usize = 96;
const OFF_AUTHORITY_BUMP: usize = 128;
const OFF_DECIMALS: usize = 129;

/// A backpointer is INITIALISED when it names a non-default unwrapped mint.
/// An all-zero buffer is a freshly allocated account, not a registered market —
/// treating the two as the same is how you end up with a registry full of
/// markets pointing at the system program.
pub fn is_initialised(data: &[u8]) -> bool {
    data.len() >= LEN && data[OFF_UNWRAPPED_MINT..OFF_ESCROW].iter().any(|b| *b != 0)
}

pub fn unwrapped_mint(data: &[u8]) -> Result<Address, ProgramError> {
    read_address(data, OFF_UNWRAPPED_MINT)
}

pub fn escrow(data: &[u8]) -> Result<Address, ProgramError> {
    read_address(data, OFF_ESCROW)
}

fn read_address(data: &[u8], off: usize) -> Result<Address, ProgramError> {
    let bytes: [u8; 32] = data
        .get(off..off + 32)
        .and_then(|s| s.try_into().ok())
        .ok_or(ProgramError::from(WrapError::NotEnoughAccounts))?;
    Ok(Address::from(bytes))
}

/// Serialise a record into an already-allocated, program-owned buffer.
#[allow(clippy::too_many_arguments)]
pub fn write(
    data: &mut [u8],
    unwrapped_mint: &Address,
    escrow: &Address,
    unwrapped_token_program: &Address,
    wrapped_token_program: &Address,
    authority_bump: u8,
    decimals: u8,
) -> Result<(), ProgramError> {
    if data.len() < LEN {
        return Err(WrapError::NotEnoughAccounts.into());
    }
    data[OFF_UNWRAPPED_MINT..OFF_UNWRAPPED_MINT + 32].copy_from_slice(unwrapped_mint.as_ref());
    data[OFF_ESCROW..OFF_ESCROW + 32].copy_from_slice(escrow.as_ref());
    data[OFF_UNWRAPPED_TOKEN_PROGRAM..OFF_UNWRAPPED_TOKEN_PROGRAM + 32]
        .copy_from_slice(unwrapped_token_program.as_ref());
    data[OFF_WRAPPED_TOKEN_PROGRAM..OFF_WRAPPED_TOKEN_PROGRAM + 32]
        .copy_from_slice(wrapped_token_program.as_ref());
    data[OFF_AUTHORITY_BUMP] = authority_bump;
    data[OFF_DECIMALS] = decimals;
    // Reserved tail stays zeroed so a later field can be added without a realloc
    // and without an old record decoding as garbage.
    for b in data[OFF_DECIMALS + 1..LEN].iter_mut() {
        *b = 0;
    }
    Ok(())
}

/// True when `account` already holds this exact record — used to make
/// initialisation idempotent instead of failing a permissionless caller who
/// raced someone else to it.
pub fn matches(data: &[u8], unwrapped: &Address, escrow_addr: &Address) -> bool {
    is_initialised(data)
        && unwrapped_mint(data).map(|m| m == *unwrapped).unwrap_or(false)
        && escrow(data).map(|e| e == *escrow_addr).unwrap_or(false)
}

/// Convenience for readers: the account must be owned by this program, or the
/// bytes are whatever an unrelated program chose to put there.
pub fn verify_owner(account: &AccountView, program_id: &Address) -> Result<(), ProgramError> {
    if unsafe { account.owner() } != program_id {
        return Err(WrapError::InvalidBackpointerOwner.into());
    }
    Ok(())
}
