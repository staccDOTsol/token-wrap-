//! Wrapped-mint configuration for a yield-bearing share token.
//!
//! Ask #3 — "any extension in/out" — is served by this trait rather than by
//! restructuring the program: `MintCustomizer` already exists precisely to let
//! a fork decide what the wrapped mint looks like. Upstream ships
//! `no_extensions`, `default_token_2022` (which forces ConfidentialTransfer)
//! and `compliance`. This adds the one the NAV model needs.
//!
//! WHAT GOES ON THE WRAPPED MINT
//! -----------------------------
//! `TransferFee`, and deliberately nothing else. Every extension is a
//! liability as well as a feature:
//!
//!   - ConfidentialTransfer hides balances, which makes the reserve/supply
//!     ratio unauditable by holders — unacceptable for a share token whose
//!     entire value proposition is that you can verify NAV yourself.
//!   - TransferHook lets a third party halt transfers, so a payment rail built
//!     on it can be frozen by someone who is not the holder.
//!   - InterestBearing rebases the DISPLAYED amount while the raw balance is
//!     unchanged, which double-counts against NAV and confuses every client.
//!
//! Both authorities are the mint-authority PDA so the fee is (a) immutable in
//! practice without a program upgrade and (b) crankable by anyone — see
//! `crank.rs` for why a keypair authority would silently break that.
//!
//! WRAPPING *OUT* OF EXTENSIONS
//! ----------------------------
//! The other direction is free and already correct upstream: wrapping a
//! Token-2022 mint that carries extensions into an SPL-Token wrapped mint
//! drops them, because SPL-Token has no extension concept. `process_wrap`
//! already nets out a source-side TransferFee before pricing, so an underlying
//! that taxes transfers cannot silently under-fund the escrow.

use {
    super::interface::MintCustomizer,
    solana_account_info::AccountInfo,
    solana_program_error::{ProgramError, ProgramResult},
    solana_pubkey::Pubkey,
    spl_token_2022_interface::{
        extension::{
            transfer_fee::instruction::initialize_transfer_fee_config, BaseStateWithExtensions,
            ExtensionType, PodStateWithExtensions,
        },
        pod::PodMint,
    },
};

/// 2 bps. Matches the EVM wrappers so the economics are identical across
/// chains rather than an accident of whichever was written last.
pub const TRANSFER_FEE_BASIS_POINTS: u16 = 20;

/// No cap.
///
/// A maximum fee makes the rate regressive — large transfers pay a smaller
/// share than small ones — and creates a threshold above which the mechanism
/// stops working. `u64::MAX` keeps it strictly proportional at every size.
pub const MAXIMUM_FEE: u64 = u64::MAX;

/// Wrapped mint carrying only `TransferFeeConfig`, with both fee authorities
/// set to the mint-authority PDA so the fee is effectively immutable and the
/// crank stays permissionless.
pub struct YieldBearingCustomizer;

impl MintCustomizer for YieldBearingCustomizer {
    fn get_token_2022_mint_space() -> Result<usize, ProgramError> {
        ExtensionType::try_calculate_account_len::<spl_token_2022_interface::state::Mint>(&[
            ExtensionType::TransferFeeConfig,
        ])
    }

    fn initialize_extensions(
        wrapped_mint_account: &AccountInfo,
        wrapped_token_program_account: &AccountInfo,
    ) -> ProgramResult {
        let (authority, _) = crate::get_wrapped_mint_authority_with_seed(wrapped_mint_account.key);
        // Both authorities are the PDA: the withdraw authority MUST be, or the
        // crank cannot complete and fees strand in the mint forever.
        let ix = initialize_transfer_fee_config(
            wrapped_token_program_account.key,
            wrapped_mint_account.key,
            Some(&authority),
            Some(&authority),
            TRANSFER_FEE_BASIS_POINTS,
            MAXIMUM_FEE,
        )?;
        // `from_ref`, not a cloned array: cloning AccountInfo here forces the
        // borrow to outlive the call and the lifetimes cannot be proven.
        solana_cpi::invoke(&ix, core::slice::from_ref(wrapped_mint_account))
    }

    fn get_freeze_auth_and_decimals(
        unwrapped_mint_account: &AccountInfo,
    ) -> Result<(Option<Pubkey>, u8), ProgramError> {
        let data = unwrapped_mint_account.try_borrow_data()?;
        let mint = PodStateWithExtensions::<PodMint>::unpack(&data)?;
        // No freeze authority, ever. A share token whose transfers can be
        // frozen is not a settlement asset — an agent's float must be spendable
        // without anyone's permission.
        //
        // Decimals mirror the underlying so 1 share ≈ 1 unit at genesis and
        // NAV reads as a plain multiple.
        Ok((None, mint.base.decimals))
    }
}
