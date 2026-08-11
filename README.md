# token-wrap — yield-bearing fork

A fork of [`solana-program/token-wrap`](https://github.com/solana-program/token-wrap)
where the wrapped token is a **share in a pool** rather than a 1:1 receipt.

Upstream is a format shim: escrow balance always equals wrapped supply, and the
two are interchangeable at par. That is exactly right for moving between SPL
Token and Token-2022, and it cannot express reserves growing.

Here:

```
wrap:    shares = assets × supply / reserves
unwrap:  assets = shares × reserves / supply
```

When reserves rise without shares being minted — or supply falls without
reserves leaving — every remaining share is worth more. That is the whole
mechanism, and it needs no custom fee code: Token-2022's `TransferFee`
extension withholds on transfer, `harvest_withheld_tokens_to_mint` is
permissionless, and burning the harvest drops supply directly.

## Two builds

| | size | notes |
|---|---|---|
| `pinocchio-program/` | **15,936 B** | `#![no_std]`, zero deps, no allocator |
| `program/` | 443,712 B | the solana-program fork, full upstream surface |

**27.8× smaller.** Rent-exempt deployment cost is `0.22 SOL` vs `6.18 SOL` —
the port pays for itself on the first deploy.

`nav.rs` is **copied, not reimplemented**, between the two. Two hand-written
copies of the same arithmetic is precisely how they drift apart.

## Design notes worth reading before changing anything

**Snapshot before mutate.** `wrap` reads reserves *before* the transfer lands;
pricing against post-transfer reserves values the depositor's own money as
already pooled and mints them too few shares. `unwrap` reads supply *before*
the burn; burning first shrinks supply and inflates that redeemer's own payout.
Same bug, mirrored.

**All rounding floors toward the pool.** Minting rounds down so a depositor
cannot mint more claim than they paid for; redeeming rounds down so nobody
withdraws more than their share. The opposite is the classic ERC-4626 drain,
one unit at a time, funded by everyone else. `round_trip_never_profits_the_caller`
asserts this across five magnitudes.

**`MINIMUM_LIQUIDITY` is locked on the first deposit.** With zero supply the
first depositor sets the price arbitrarily — deposit 1 unit, donate a large
balance straight to the escrow, and every later depositor's shares round to
zero. Uniswap V2 locks liquidity for the same reason.

**The fee authority must be a PDA.** `harvest_withheld_tokens_to_mint` is
permissionless, but `withdraw_withheld_tokens_from_mint` requires the mint's
`WithdrawWithheldAuthority` to sign. A keypair there means yield stops whenever
its holder is offline. A PDA lets the *program* sign, so anyone can complete
the cycle.

**The crank's odd unit goes to the burn.** Fees split 50/50 burn/treasury; a
dev share that rounded *up* would let a caller crank dust repeatedly and skim.
A burn cannot be extracted, which is what keeps the crank safe to leave open.

**Bumps are passed, not derived.** `find_program_address` is deliberately
absent from Pinocchio — on-chain derivation costs ~1,500 CU per attempt. The
bump travels in instruction data and `invoke_signed` rejects a forged one, so
the runtime does the check for free.

**The wrapped mint carries only `TransferFeeConfig`.** The omissions are the
point: `ConfidentialTransfer` makes the reserve/supply ratio unauditable by
holders, `TransferHook` lets a third party freeze a payment rail, and
`InterestBearing` rebases the displayed amount and double-counts against NAV.
No freeze authority, ever.

## Instruction encoding

No IDL, no Borsh — a single-byte discriminant.

```
0  CreateMint
1  Wrap        [u8 tag][u64 amount LE][u8 bump]
2  Unwrap      [u8 tag][u64 shares LE][u8 bump]
3  CrankFees   [u8 tag][u8 bump]
```

## Status

Deployed to Solana mainnet and devnet at
`FrSERTNCPvTtaDS9AvQp9u1nYGzXDb3kC9MdL8Xxn2NE` (upgradeable).

**13 host tests pass. There are zero integration tests.** No CPI path has ever
executed against a validator — the NAV arithmetic is well covered, the account
wiring is not. Treat it accordingly.

Unaudited.

## Build

```sh
cargo test                                            # host tests
cargo-build-sbf --manifest-path pinocchio-program/Cargo.toml
```

Requires a recent `cargo-build-sbf` (4.x / platform-tools ≥1.54); older
toolchains fail to resolve dependencies.
