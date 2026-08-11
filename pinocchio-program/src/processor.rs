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

    /// Token-2022 extension instructions are two bytes: the extension's outer
    /// discriminant, then a sub-instruction. TransferFee is 26.
    pub const TRANSFER_FEE_EXT: u8 = 26;
    /// Move withheld fees from the MINT to an account. Authority-gated —
    /// requires the mint's WithdrawWithheldAuthority to sign.
    pub const WITHDRAW_WITHHELD_FROM_MINT: u8 = 2;
    /// Sweep withheld fees from holder accounts INTO the mint. Permissionless
    /// by design: no authority, anyone may call it.
    pub const HARVEST_TO_MINT: u8 = 4;
}

/// Extra accounts a Token-2022 TransferHook needs, forwarded verbatim.
///
/// A hook-gated mint — which is how every serious RWA enforces an allowlist —
/// cannot be transferred at all unless the hook program and its
/// `ExtraAccountMetaList` ride along with the CPI. Without this, wrapping such
/// a token fails on the escrow deposit, which is precisely the class of token
/// this program is for.
///
/// Bounded because `no_allocator!` forbids Vec. Ten covers the hook program,
/// its meta list, and a realistic allowlist lookup; more than that and the
/// caller should be splitting the work anyway.
pub const MAX_HOOK_ACCOUNTS: usize = 10;

/// Build a CPI account list of `fixed` metas plus however many hook accounts
/// were passed, without allocating.
fn with_hook_accounts<'a>(
    fixed: &[InstructionAccount<'a>],
    hooks: &'a [AccountView],
    out: &mut [InstructionAccount<'a>; MAX_HOOK_ACCOUNTS + 4],
) -> usize {
    let mut n = 0;
    for m in fixed {
        out[n] = InstructionAccount {
            address: m.address,
            is_writable: m.is_writable,
            is_signer: m.is_signer,
        };
        n += 1;
    }
    for h in hooks.iter().take(MAX_HOOK_ACCOUNTS) {
        // Forwarded read-only and unsigned: a hook may inspect accounts, but
        // nothing it is handed should gain write or signer authority it did
        // not already have from the outer transaction.
        out[n] = InstructionAccount::readonly(h.address());
        n += 1;
    }
    n
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
    // NO hook forwarding here, deliberately. On the way IN the caller moves the
    // unwrapped tokens into escrow with their own transfer instruction, so the
    // issuer's TransferHook fires on THAT instruction, against the caller's own
    // account list — this program never CPIs the unwrapped mint during a wrap.
    // It only mints shares, and the wrapped mint is ours. `unwrap` is different:
    // there the escrow release IS our CPI, so it forwards trailing accounts (see
    // `with_hook_accounts`). An earlier version bound `&accounts[5..]` here and
    // never used it, which read as if wrap forwarded hooks too.
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
///   3 `[signer]`   holder — OWNER of account 2
///   4 `[writable]` recipient unwrapped token account
///   5 `[]`         wrapped mint authority PDA
///   6 `[]`         unwrapped mint
///   7 `[]`         token program
///
/// TWO DIFFERENT SIGNERS, AND THEY ARE NOT INTERCHANGEABLE.
///
/// The burn is authorised by the HOLDER, because a token account can only be
/// debited by its owner — signing the burn with the program's PDA fails with
/// `owner does not match`, which is precisely what devnet returned the first
/// time this ran. The escrow release is authorised by the PDA, because the
/// escrow is owned by the program.
///
/// Host tests cannot catch this: the arithmetic is identical either way and
/// only a validator enforces account ownership.
pub fn unwrap(
    _program_id: &Address,
    accounts: &[AccountView],
    shares: u64,
    bump: u8,
) -> ProgramResult {
    need(accounts, 8)?;
    let hook_accounts = &accounts[8..];
    let escrow = &accounts[0];
    let wrapped_mint = &accounts[1];
    let holder = &accounts[2];
    let holder_authority = &accounts[3];
    let recipient = &accounts[4];
    let authority = &accounts[5];
    let unwrapped_mint = &accounts[6];
    let token_program = &accounts[7];

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
        InstructionAccount::readonly_signer(holder_authority.address()),
    ];
    // `invoke`, not `invoke_signed`: the holder already signed the outer
    // transaction, so no PDA seeds are involved in the burn at all.
    pinocchio::cpi::invoke(
        &InstructionView {
            program_id: token_program.address(),
            accounts: &bmetas,
            data: &bdata,
        },
        &[holder, wrapped_mint, holder_authority],
    )?;

    let decimals = mint_decimals(unwrapped_mint)?;
    let mut tdata = [0u8; 10];
    tdata[0] = tix::TRANSFER_CHECKED;
    tdata[1..9].copy_from_slice(&assets_out.to_le_bytes());
    tdata[9] = decimals;
    let tfixed = [
        InstructionAccount::writable(escrow.address()),
        InstructionAccount::readonly(unwrapped_mint.address()),
        InstructionAccount::writable(recipient.address()),
        InstructionAccount::readonly_signer(authority.address()),
    ];
    let mut tbuf: [InstructionAccount; MAX_HOOK_ACCOUNTS + 4] =
        core::array::from_fn(|_| InstructionAccount::readonly(escrow.address()));
    let tn = with_hook_accounts(&tfixed, hook_accounts, &mut tbuf);

    // The account VIEWS must mirror the metas one-for-one, or the runtime
    // cannot resolve what the instruction refers to.
    let mut tviews: [&AccountView; MAX_HOOK_ACCOUNTS + 4] =
        core::array::from_fn(|_| escrow);
    tviews[0] = escrow;
    tviews[1] = unwrapped_mint;
    tviews[2] = recipient;
    tviews[3] = authority;
    for (i, h) in hook_accounts.iter().take(MAX_HOOK_ACCOUNTS).enumerate() {
        tviews[4 + i] = h;
    }

    pinocchio::cpi::invoke_signed_with_bounds::<{ MAX_HOOK_ACCOUNTS + 4 }, _>(
        &InstructionView {
            program_id: token_program.address(),
            accounts: &tbuf[..tn],
            data: &tdata,
        },
        &tviews[..tn],
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
    // Everything past the fixed five is a holder account to harvest from.
    let sources = &accounts[5..];

    let bump_arr = [bump];
    let seeds = [
        Seed::from(AUTHORITY_SEED),
        Seed::from(wrapped_mint.address().as_ref()),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];

    // ── Step 1: harvest holder accounts into the mint.
    //
    // Permissionless — no authority signs this. Token-2022 accumulates
    // withheld fees in RECIPIENT accounts rather than one place, deliberately,
    // so a single fee account never becomes a write-lock bottleneck. The cost
    // is that collecting them requires visiting each account, which is why
    // this instruction is variadic and why the crank exists at all.
    //
    // Skipped when no sources are passed: a caller may only want to sweep what
    // has already reached the mint.
    // Bounded to MAX_HARVEST per call — no_allocator! means no Vec, and a
    // fixed bound is the better design anyway: an unbounded account list makes
    // compute cost unpredictable and can blow the budget mid-crank, leaving
    // fees half-swept. Callers with more accounts crank in batches.
    const MAX_HARVEST: usize = 8;
    let n = if sources.len() > MAX_HARVEST { MAX_HARVEST } else { sources.len() };
    if n > 0 {
        // `InstructionAccount` is not Copy, so the array is built by index
        // rather than by repeat-initialiser.
        let metas: [InstructionAccount; MAX_HARVEST + 1] = core::array::from_fn(|i| {
            if i == 0 {
                InstructionAccount::writable(wrapped_mint.address())
            } else if i <= n {
                InstructionAccount::writable(sources[i - 1].address())
            } else {
                // Padding past `n`; never passed to the CPI, which slices to
                // n + 1.
                InstructionAccount::readonly(wrapped_mint.address())
            }
        });
        let data = [tix::TRANSFER_FEE_EXT, tix::HARVEST_TO_MINT];

        // Account views mirror the metas, same order, same count.
        let mut infos: [&AccountView; MAX_HARVEST + 1] = [wrapped_mint; MAX_HARVEST + 1];
        for (i, a) in sources[..n].iter().enumerate() {
            infos[i + 1] = a;
        }

        pinocchio::cpi::invoke_signed_with_bounds::<{ MAX_HOOK_ACCOUNTS + 4 }, _>(
            &InstructionView {
                program_id: token_program.address(),
                accounts: &metas[..n + 1],
                data: &data,
            },
            &infos[..n + 1],
            &[],
        )?;
    }

    // ── Step 2: withdraw the mint's withheld balance to scratch.
    //
    // THIS is why the WithdrawWithheldAuthority must be the PDA. Step 1 is
    // open to anyone, but this one demands a signature — so a keypair here
    // means the cycle only completes while its holder is online, and the yield
    // stalls whenever they are not. A PDA lets the program sign, keeping the
    // whole crank permissionless end to end.
    let wmetas = [
        InstructionAccount::writable(wrapped_mint.address()),
        InstructionAccount::writable(scratch.address()),
        InstructionAccount::readonly_signer(authority.address()),
    ];
    let wdata = [tix::TRANSFER_FEE_EXT, tix::WITHDRAW_WITHHELD_FROM_MINT];
    invoke_signed(
        &InstructionView {
            program_id: token_program.address(),
            accounts: &wmetas,
            data: &wdata,
        },
        &[wrapped_mint, scratch, authority],
        &signer,
    )?;

    // ── Step 3: split and burn.
    //
    // Read AFTER the withdraw: the balance is whatever actually arrived, not
    // what we predicted. Zero is not an error — a crank that reverts when idle
    // cannot be run on a schedule.
    let pending = token_account_amount(scratch)?;
    if pending == 0 {
        return Ok(());
    }

    // The dev share rounds DOWN and the burn takes the remainder, so a caller
    // cranking dust repeatedly can never round the treasury upward. Neither
    // half is payable to the caller, which is what keeps this safe to leave
    // open to anyone.
    let to_treasury = pending / 2;
    let to_burn = pending
        .checked_sub(to_treasury)
        .ok_or(ProgramError::from(WrapError::ArithmeticOverflow))?;

    let decimals = mint_decimals(wrapped_mint)?;

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

/// Register a wrapped mint in the on-chain registry.
///
/// Accounts:
///   0 `[writable]` backpointer PDA — PRE-FUNDED by the caller with at least
///                  rent-exempt lamports for `crate::backpointer::LEN`. Funding it
///                  outside keeps System-rent maths out of this program, the
///                  same trade `create_mint` makes for the mint itself.
///   1 `[]`         wrapped mint
///   2 `[]`         escrow (reserves) token account
///   3 `[]`         unwrapped mint
///   4 `[]`         unwrapped token program
///   5 `[]`         wrapped token program
///   6 `[]`         system program — REQUIRED. A CPI target must itself appear
///                  in the transaction's account list; without it the runtime
///                  reports "Unknown program 111…111" and the instruction fails
///                  with MissingRequiredAccount before Allocate ever runs.
///
/// Data: `bump` (backpointer PDA bump).
///
/// PERMISSIONLESS AND IDEMPOTENT: anyone may register, and re-registering the
/// same pair is a no-op rather than an error, so two clients racing to index a
/// new market both succeed. Registering a DIFFERENT pair over an existing
/// record is rejected — that would silently repoint a live market.
///
/// WHY THIS IS SAFE WITHOUT DERIVING THE AUTHORITY. Pinocchio deliberately
/// ships no `find_program_address`/`create_program_address`, so the program
/// cannot recompute the mint-authority PDA to check the escrow belongs to it.
/// It does not need to: the wrapped mint's OWN `mint_authority` field is that
/// PDA (set at creation, and `wrap` already rejects the mint outright if it is
/// not — see MintAuthorityMismatch). Requiring `escrow.owner == wrapped_mint
/// .mint_authority` binds the escrow to whatever can mint this share token,
/// which is exactly the relationship the registry is asserting. The
/// backpointer's own address is validated by the runtime when `invoke_signed`
/// checks the seeds below, so a forged bump costs nothing to reject.
pub fn init_backpointer(
    program_id: &Address,
    accounts: &mut [AccountView],
    bump: u8,
) -> ProgramResult {
    need(accounts, 7)?;
    // `try_borrow_mut` needs `&mut AccountView`, and indexing the slice twice
    // would hand out a mutable and a shared borrow of the same slice. Splitting
    // once gives the writable backpointer and the read-only rest, with the
    // borrow checker satisfied and no clone.
    let (backpointer, rest) = accounts
        .split_first_mut()
        .ok_or(ProgramError::from(WrapError::NotEnoughAccounts))?;
    let wrapped_mint = &rest[0];
    let escrow = &rest[1];
    let unwrapped_mint = &rest[2];
    let unwrapped_token_program = &rest[3];
    let wrapped_token_program = &rest[4];

    // ── bind the escrow to this wrapped mint ──────────────────────────────
    let (esc_mint, esc_owner, _) = {
        let d = escrow.try_borrow()?;
        let m: [u8; 32] = d.get(0..32).and_then(|s| s.try_into().ok())
            .ok_or(ProgramError::from(WrapError::NotEnoughAccounts))?;
        let o: [u8; 32] = d.get(32..64).and_then(|s| s.try_into().ok())
            .ok_or(ProgramError::from(WrapError::NotEnoughAccounts))?;
        (Address::from(m), Address::from(o), ())
    };
    if esc_mint != *unwrapped_mint.address() {
        return Err(WrapError::BackpointerEscrowMismatch.into());
    }
    // Mint layout: COption<Address> mint_authority = 4-byte tag + 32-byte body.
    let (mint_authority, decimals) = {
        let d = wrapped_mint.try_borrow()?;
        let a: [u8; 32] = d.get(4..36).and_then(|s| s.try_into().ok())
            .ok_or(ProgramError::from(WrapError::NotEnoughAccounts))?;
        let dec = *d.get(crate::state::MINT_DECIMALS_OFFSET)
            .ok_or(ProgramError::from(WrapError::NotEnoughAccounts))?;
        (Address::from(a), dec)
    };
    if esc_owner != mint_authority {
        return Err(WrapError::BackpointerEscrowMismatch.into());
    }

    // ── idempotence: already registered? ──────────────────────────────────
    if unsafe { backpointer.owner() } == program_id {
        let d = backpointer.try_borrow()?;
        if crate::backpointer::is_initialised(&d) {
            return if crate::backpointer::matches(&d, unwrapped_mint.address(), escrow.address()) {
                Ok(())
            } else {
                Err(WrapError::BackpointerConflict.into())
            };
        }
    }

    // ── allocate + assign, signed by the backpointer PDA ──────────────────
    // Allocate(space) then Assign(owner) rather than CreateAccount, so the
    // program never needs the rent sysvar or a lamport figure: the caller
    // pre-funds the address and these two only change size and ownership.
    let bump_arr = [bump];
    let seeds = [
        Seed::from(crate::backpointer::BACKPOINTER_SEED),
        Seed::from(wrapped_mint.address().as_ref()),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    let system = Address::from([0u8; 32]);

    if unsafe { backpointer.owner() } != program_id {
        let mut alloc = [0u8; 12];
        alloc[0] = 8; // System::Allocate
        alloc[4..12].copy_from_slice(&(crate::backpointer::LEN as u64).to_le_bytes());
        let metas = [InstructionAccount::writable_signer(backpointer.address())];
        invoke_signed(
            &InstructionView { program_id: &system, accounts: &metas, data: &alloc },
            &[&*backpointer],
            &signer,
        )?;

        let mut assign = [0u8; 36];
        assign[0] = 1; // System::Assign
        assign[4..36].copy_from_slice(program_id.as_ref());
        let metas = [InstructionAccount::writable_signer(backpointer.address())];
        invoke_signed(
            &InstructionView { program_id: &system, accounts: &metas, data: &assign },
            &[&*backpointer],
            &signer,
        )?;
    }

    let mut d = backpointer.try_borrow_mut()?;
    crate::backpointer::write(
        &mut d,
        unwrapped_mint.address(),
        escrow.address(),
        unwrapped_token_program.address(),
        wrapped_token_program.address(),
        0, // authority bump is not derivable here; clients read mint_authority
        decimals,
    )
}
