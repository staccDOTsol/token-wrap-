//! The four instructions.
//!
//! Every one snapshots pool state BEFORE it mutates anything — the ordering
//! bug that lets a depositor price against their own money, or a redeemer
//! inflate their own payout by burning first.
//!
//! BUMPS ARE PASSED IN, NOT DERIVED.
//!
//! `find_program_address` is deliberately absent from Pinocchio: deriving a PDA
//! on-chain costs roughly 1,500 CU per attempt, and an off-curve search can
//! take several. The idiom is to pass the bump in instruction data and let the
//! runtime validate it — `invoke_signed` fails if the seeds do not produce the
//! account being asked to sign, so a forged bump cannot authorise anything. The
//! check is free because the runtime does it anyway.

use {
    crate::{
        error::WrapError,
        nav,
        state::{mint_decimals, mint_supply, token_account_amount},
    },
    pinocchio::{
        account::AccountView,
        address::Address,
        cpi::{invoke_signed, Seed, Signer},
        error::{ProgramError, ProgramResult},
        instruction::{InstructionAccount, InstructionView},
    },
};

/// Seed for the mint-authority PDA. One authority for minting, burning, fee
/// config and fee withdrawal — see `crank_fees` for why they must not differ.
pub const AUTHORITY_SEED: &[u8] = b"mint_authority";

/// Token instruction discriminants, hand-encoded.
///
/// Encoding four instructions by hand instead of linking a CPI crate is most
/// of why this binary is small — and the layouts are frozen by the token
/// program, so there is nothing to track.
mod tix {
    pub const TRANSFER_CHECKED: u8 = 12;
    pub const MINT_TO: u8 = 7;
    pub const BURN: u8 = 8;
}

fn need(accounts: &[AccountView], n: usize) -> Result<(), ProgramError> {
    if accounts.len() < n {
        return Err(WrapError::NotEnoughAccounts.into());
    }
    Ok(())
}

/// Create the wrapped mint and escrow.
///
/// Account creation itself is left to the caller: doing it here would mean
/// carrying System-program CPI and rent maths for a one-off setup step that a
/// client can do in the same transaction.
pub fn create_mint(_program_id: &Address, accounts: &[AccountView]) -> ProgramResult {
    need(accounts, 3)
}

/// Deposit unwrapped tokens, mint shares at NAV.
///
/// Accounts:
///   0 `[writable]` unwrapped escrow (reserves)
///   1 `[writable]` wrapped mint (supply)
///   2 `[writable]` recipient wrapped token account
///   3 `[]`         wrapped mint authority PDA
///   4 `[]`         token program
pub fn wrap(_program_id: &Address, accounts: &[AccountView], amount: u64, bump: u8) -> ProgramResult {
    need(accounts, 5)?;
    let escrow = &accounts[0];
    let wrapped_mint = &accounts[1];
    let recipient = &accounts[2];
    let authority = &accounts[3];
    let token_program = &accounts[4];

    // BEFORE the deposit lands. Pricing against post-transfer reserves values
    // the depositor's own money as already pooled and mints them too few.
    let reserves_before = token_account_amount(escrow)?;
    let supply_before = mint_supply(wrapped_mint)?;

    let shares = nav::shares_for_assets(amount, reserves_before, supply_before)?;
    if shares == 0 {
        // Would be a silent donation to existing holders. Refuse.
        return Err(WrapError::DepositTooSmall.into());
    }

    let mut data = [0u8; 9];
    data[0] = tix::MINT_TO;
    data[1..9].copy_from_slice(&shares.to_le_bytes());

    let metas = [
        InstructionAccount::writable(wrapped_mint.address()),
        InstructionAccount::writable(recipient.address()),
        InstructionAccount::readonly_signer(authority.address()),
    ];
    let ix = InstructionView {
        program_id: token_program.address(),
        accounts: &metas,
        data: &data,
    };

    let bump_arr = [bump];
    let seeds = [
        Seed::from(AUTHORITY_SEED),
        Seed::from(wrapped_mint.address().as_ref()),
        Seed::from(&bump_arr[..]),
    ];
    invoke_signed(
        &ix,
        &[wrapped_mint, recipient, authority],
        &[Signer::from(&seeds[..])],
    )
}

/// Burn shares, release unwrapped tokens at NAV.
///
/// Accounts:
///   0 `[writable]` unwrapped escrow (reserves)
///   1 `[writable]` wrapped mint (supply)
///   2 `[writable]` holder wrapped token account
///   3 `[writable]` recipient unwrapped token account
///   4 `[]`         wrapped mint authority PDA
///   5 `[]`         unwrapped mint
///   6 `[]`         token program
pub fn unwrap(
    _program_id: &Address,
    accounts: &[AccountView],
    shares: u64,
    bump: u8,
) -> ProgramResult {
    need(accounts, 7)?;
    let escrow = &accounts[0];
    let wrapped_mint = &accounts[1];
    let holder = &accounts[2];
    let recipient = &accounts[3];
    let authority = &accounts[4];
    let unwrapped_mint = &accounts[5];
    let token_program = &accounts[6];

    // BEFORE the burn. Burning first shrinks supply and inflates this
    // redeemer's own payout — the mirror of the wrap ordering bug.
    let reserves_before = token_account_amount(escrow)?;
    let supply_before = mint_supply(wrapped_mint)?;

    let assets_out = nav::assets_for_shares(shares, reserves_before, supply_before)?;
    if assets_out == 0 {
        return Err(WrapError::RedeemTooSmall.into());
    }

    let bump_arr = [bump];
    let seeds = [
        Seed::from(AUTHORITY_SEED),
        Seed::from(wrapped_mint.address().as_ref()),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];

    // Burn BEFORE releasing. Both are in one transaction, so a failed transfer
    // reverts the burn with it — but the reverse order would briefly leave the
    // pool short if anything between them could observe state.
    let mut bdata = [0u8; 9];
    bdata[0] = tix::BURN;
    bdata[1..9].copy_from_slice(&shares.to_le_bytes());
    let bmetas = [
        InstructionAccount::writable(holder.address()),
        InstructionAccount::writable(wrapped_mint.address()),
        InstructionAccount::readonly_signer(authority.address()),
    ];
    invoke_signed(
        &InstructionView {
            program_id: token_program.address(),
            accounts: &bmetas,
            data: &bdata,
        },
        &[holder, wrapped_mint, authority],
        &signer,
    )?;

    let decimals = mint_decimals(unwrapped_mint)?;
    let mut tdata = [0u8; 10];
    tdata[0] = tix::TRANSFER_CHECKED;
    tdata[1..9].copy_from_slice(&assets_out.to_le_bytes());
    tdata[9] = decimals;
    let tmetas = [
        InstructionAccount::writable(escrow.address()),
        InstructionAccount::readonly(unwrapped_mint.address()),
        InstructionAccount::writable(recipient.address()),
        InstructionAccount::readonly_signer(authority.address()),
    ];
    invoke_signed(
        &InstructionView {
            program_id: token_program.address(),
            accounts: &tmetas,
            data: &tdata,
        },
        &[escrow, unwrapped_mint, recipient, authority],
        &signer,
    )
}

/// Harvest withheld transfer fees; split 50/50 burn / treasury.
///
/// The withdraw step needs the mint's WithdrawWithheldAuthority to sign, which
/// is why that authority must be this PDA: a keypair there stalls the crank
/// whenever its holder is offline, and the yield stalls with it.
///
/// The odd unit goes to the BURN. A dev share that rounded up would let a
/// caller crank tiny amounts repeatedly and skim; a burn cannot be extracted,
/// which is what keeps this safe to leave open to anyone.
///
/// Accounts:
///   0 `[writable]` wrapped mint
///   1 `[writable]` scratch token account owned by the authority PDA
///   2 `[]`         wrapped mint authority PDA
///   3 `[]`         token program
///   4 `[writable]` treasury token account
pub fn crank_fees(_program_id: &Address, accounts: &[AccountView], bump: u8) -> ProgramResult {
    need(accounts, 5)?;
    let wrapped_mint = &accounts[0];
    let scratch = &accounts[1];
    let authority = &accounts[2];
    let token_program = &accounts[3];
    let treasury = &accounts[4];

    // Whatever the harvest step has already swept into the scratch account.
    // Zero is not an error: a crank that reverts when idle cannot be run on a
    // schedule.
    let pending = token_account_amount(scratch)?;
    if pending == 0 {
        return Ok(());
    }

    let to_treasury = pending / 2;
    let to_burn = pending
        .checked_sub(to_treasury)
        .ok_or(ProgramError::from(WrapError::ArithmeticOverflow))?;

    let decimals = mint_decimals(wrapped_mint)?;
    let bump_arr = [bump];
    let seeds = [
        Seed::from(AUTHORITY_SEED),
        Seed::from(wrapped_mint.address().as_ref()),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];

    if to_treasury > 0 {
        let mut tdata = [0u8; 10];
        tdata[0] = tix::TRANSFER_CHECKED;
        tdata[1..9].copy_from_slice(&to_treasury.to_le_bytes());
        tdata[9] = decimals;
        let metas = [
            InstructionAccount::writable(scratch.address()),
            InstructionAccount::readonly(wrapped_mint.address()),
            InstructionAccount::writable(treasury.address()),
            InstructionAccount::readonly_signer(authority.address()),
        ];
        invoke_signed(
            &InstructionView {
                program_id: token_program.address(),
                accounts: &metas,
                data: &tdata,
            },
            &[scratch, wrapped_mint, treasury, authority],
            &signer,
        )?;
    }

    let mut bdata = [0u8; 9];
    bdata[0] = tix::BURN;
    bdata[1..9].copy_from_slice(&to_burn.to_le_bytes());
    let bmetas = [
        InstructionAccount::writable(scratch.address()),
        InstructionAccount::writable(wrapped_mint.address()),
        InstructionAccount::readonly_signer(authority.address()),
    ];
    invoke_signed(
        &InstructionView {
            program_id: token_program.address(),
            accounts: &bmetas,
            data: &bdata,
        },
        &[scratch, wrapped_mint, authority],
        &signer,
    )
}
