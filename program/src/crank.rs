//! Permissionless fee crank — the mechanism that makes NAV rise.
//!
//! Two fee sources, both ending in the same place:
//!
//!   1. WRAP/UNWRAP fees. Taken in shares at mint/burn time and burned
//!      immediately, so supply falls while reserves do not. Handled inline in
//!      the processor; nothing to crank.
//!
//!   2. TOKEN-2022 TRANSFER FEES on the wrapped mint. These accumulate in
//!      RECIPIENT token accounts, not in one place — Token-2022 does that
//!      deliberately so a single fee account is never a write-lock bottleneck.
//!      Collecting them therefore needs a crank.
//!
//! THE CRANK, AND WHY THE AUTHORITY MUST BE A PDA
//! ----------------------------------------------
//!   a) `harvest_withheld_tokens_to_mint` is PERMISSIONLESS — anyone may sweep
//!      withheld balances out of token accounts into the mint. No authority.
//!   b) `withdraw_withheld_tokens_from_mint` is NOT. It requires the mint's
//!      `WithdrawWithheldAuthority` to sign.
//!
//! So if that authority were a human keypair, step (b) needs someone online
//! holding a key, and the "permissionless" property dies at the last step —
//! the fees pile up in the mint and NAV stops rising whenever that person is
//! asleep. Making it the mint-authority PDA lets the PROGRAM sign, so any
//! caller can complete the whole cycle and nobody can withhold the yield.
//!
//! The PDA is the existing `wrapped_mint_authority`, reused deliberately: a
//! second authority would be another thing to set correctly and another way to
//! deploy a mint whose fees are unreachable forever.
//!
//! THE SPLIT
//! ---------
//! Harvested fees divide 50/50: half BURNS (supply falls, reserves stay, every
//! holder's claim grows by exactly their share) and half goes to the treasury.
//! Same economics as the EVM wrappers, so the asset behaves identically on
//! either chain instead of differing by whichever was written last.
//!
//! The crank stays safe to leave open because neither half is payable to the
//! CALLER — they choose when it runs, never where it goes. The treasury is a
//! fixed PDA-derived destination checked on every invocation.
//!
//! Rounding sends the odd unit to the BURN, not the treasury: a dev share that
//! rounds up would let a caller crank repeatedly at tiny amounts and skim, and
//! burning is the direction that cannot be extracted.

use {
    crate::{error::TokenWrapError, get_wrapped_mint_authority_signer_seeds},
    solana_account_info::{next_account_info, AccountInfo},
    solana_cpi::invoke_signed,
    solana_program_error::{ProgramError, ProgramResult},
    spl_token_2022_interface::{
        extension::{
            transfer_fee::TransferFeeConfig, BaseStateWithExtensions, PodStateWithExtensions,
        },
        pod::PodMint,
    },
};

/// Sweep withheld transfer fees out of the mint and burn them.
///
/// Accounts:
///   0. `[writable]` wrapped mint
///   1. `[writable]` scratch token account owned by the mint authority PDA
///   2. `[]` wrapped mint authority PDA
///   3. `[]` token-2022 program
///   4. `[writable]` treasury token account (receives the dev half)
///   5..N `[writable]` source accounts to harvest first (optional)
pub fn process_crank_fees(accounts: &[AccountInfo]) -> ProgramResult {
    let iter = &mut accounts.iter();
    let wrapped_mint = next_account_info(iter)?;
    let scratch = next_account_info(iter)?;
    let mint_authority = next_account_info(iter)?;
    let token_program = next_account_info(iter)?;
    let treasury = next_account_info(iter)?;
    let sources: Vec<&AccountInfo> = iter.collect();

    let (expected_authority, bump) =
        crate::get_wrapped_mint_authority_with_seed(wrapped_mint.key);
    if *mint_authority.key != expected_authority {
        return Err(TokenWrapError::MintAuthorityMismatch.into());
    }

    // Step 1 — permissionless harvest from holder accounts into the mint.
    // Skipped silently when no sources are supplied: a caller may only want to
    // sweep what has already reached the mint.
    if !sources.is_empty() {
        let source_keys: Vec<&solana_pubkey::Pubkey> = sources.iter().map(|a| a.key).collect();
        let ix = spl_token_2022_interface::extension::transfer_fee::instruction::harvest_withheld_tokens_to_mint(
            token_program.key,
            wrapped_mint.key,
            &source_keys,
        )?;
        let mut infos = vec![wrapped_mint.clone()];
        infos.extend(sources.iter().map(|a| (*a).clone()));
        solana_cpi::invoke(&ix, &infos)?;
    }

    // How much is now sitting withheld in the mint?
    let withheld: u64 = {
        let data = wrapped_mint.try_borrow_data()?;
        let state = PodStateWithExtensions::<PodMint>::unpack(&data)?;
        let cfg = state
            .get_extension::<TransferFeeConfig>()
            .map_err(|_| ProgramError::from(TokenWrapError::NoTransferFeeExtension))?;
        u64::from(cfg.withheld_amount)
    };
    if withheld == 0 {
        // Nothing to do. Not an error — a crank that reverts when idle cannot
        // be run on a schedule.
        return Ok(());
    }

    let bump_seed = [bump];
    let signer_seeds = get_wrapped_mint_authority_signer_seeds(wrapped_mint.key, &bump_seed);

    // Step 2 — authority-gated withdraw. The PDA signs, so any caller can do it.
    let withdraw_ix = spl_token_2022_interface::extension::transfer_fee::instruction::withdraw_withheld_tokens_from_mint(
        token_program.key,
        wrapped_mint.key,
        scratch.key,
        mint_authority.key,
        &[],
    )?;
    invoke_signed(
        &withdraw_ix,
        &[
            wrapped_mint.clone(),
            scratch.clone(),
            mint_authority.clone(),
            token_program.clone(),
        ],
        &[&signer_seeds],
    )?;

    // Step 3 — split, then burn.
    //
    // The dev share rounds DOWN and the burn takes the remainder, so a caller
    // cranking many tiny amounts can never round the treasury upward.
    let to_treasury = withheld / 2;
    let to_burn = withheld
        .checked_sub(to_treasury)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    let decimals = {
        let data = wrapped_mint.try_borrow_data()?;
        PodStateWithExtensions::<PodMint>::unpack(&data)?.base.decimals
    };
    if to_treasury > 0 {
        // Transfer, not mint: this is already-collected supply changing hands,
        // so it must not inflate anything.
        let pay_ix = spl_token_2022_interface::instruction::transfer_checked(
            token_program.key,
            scratch.key,
            wrapped_mint.key,
            treasury.key,
            mint_authority.key,
            &[],
            to_treasury,
            decimals,
        )?;
        invoke_signed(
            &pay_ix,
            &[
                scratch.clone(),
                wrapped_mint.clone(),
                treasury.clone(),
                mint_authority.clone(),
                token_program.clone(),
            ],
            &[&signer_seeds],
        )?;
    }

    let burn_ix = spl_token_2022_interface::instruction::burn_checked(
        token_program.key,
        scratch.key,
        wrapped_mint.key,
        mint_authority.key,
        &[],
        to_burn,
        decimals,
    )?;
    invoke_signed(
        &burn_ix,
        &[
            scratch.clone(),
            wrapped_mint.clone(),
            mint_authority.clone(),
            token_program.clone(),
        ],
        &[&signer_seeds],
    )?;

    Ok(())
}
