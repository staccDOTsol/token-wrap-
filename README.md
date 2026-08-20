# token-wrap — yield-bearing fork

> **On the name:** this was meant to be `token-wrap++`. GitHub strips `+` from
> repo names, so the plusses collapsed and left a lonely trailing dash. It is
> `token-wrap-` forever now. Read it as `token-wrap++`.

## Why this exists

On 9 July 2026 I asked, publicly:

> *"can you bls get a wrapped spl-only variant for your coins? i set up
> FluxBeam pools for your heavily laden t22 tokens and alas even tho they
> existed arbs didn't flow. Please, for the good of the eco — make a canonical
> token wrap implementation (someone)."*

Nobody did. This is it.

**The problem is routing, not liquidity.** A Token-2022 mint carrying
extensions — transfer fees, hooks, confidential transfer — is something most
AMMs, aggregators and market makers simply will not touch. Not because the
token is bad, but because every extension is another edge case in their
execution path, and the expected volume never justifies the integration. So
you can stand up pools for a heavily-extended token, fund both sides, and
watch arbs decline to show up. The pools exist. The flow doesn't.

Wrapping to a plain SPL variant fixes that: routers handle it because there is
nothing special to handle. That is what upstream
[`solana-program/token-wrap`](https://github.com/solana-program/token-wrap)
does, and it is genuinely the right primitive.

**This fork adds the half that was missing.** Upstream's wrapper is a 1:1
receipt, so the wrapped token is inert — it routes, and that is all it does.
Here the wrapper is a *share*, so the same act of making a token routable also
makes it earn. Reserves and supply move independently, and Token-2022's own
`TransferFee` extension funds the growth.

The uncomfortable observation behind the original tweet still stands: compliance
tokens promised an ecosystem and shipped an integration burden. A canonical
wrap is the cheapest way to make that burden somebody else's problem — once —
instead of every venue's problem, forever.

## TL;DR

Upstream `token-wrap` gives you a **receipt**: put in 100, get 100, always 1:1.

This gives you a **share**. Reserves and supply move independently, so when the
pool grows without new shares being minted, every existing share is worth more.

```
wrap:    shares = assets × supply / reserves
unwrap:  assets = shares × reserves / supply
```

Where the growth comes from: Token-2022's `TransferFee` extension takes a cut
of every transfer of the wrapped token. A permissionless crank sweeps those
fees and burns half — supply falls, reserves don't move, everyone's share goes
up. The other half goes to a treasury.

**It is not a ponzi, and the difference is structural, not rhetorical.** The
yield is paid by *transfer volume*, not by new deposits. A depositor's money
goes into the escrow and stays claimable by them at all times — nobody's
deposit funds anyone else's return. If volume stops, NAV simply stops rising;
it never needs a next buyer to hold up, and there is nothing to unwind. Wrap
and unwrap are symmetric at the same price, so being early confers no
advantage over being late.

What you actually get for holding it: an idle balance that earns instead of
sitting still, on a token that stays spendable the whole time.

Also **28× smaller** than the upstream build — 15,936 vs 443,712 bytes, which
is 0.22 vs 6.18 SOL of rent locked forever.

---

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

## Wrapping OUT of extensions

The direction that motivated this is the boring one, and it works because
SPL-Token has no extension concept at all:

```
T22 mint (fees, hooks, confidential)  ->  plain SPL wrapped mint
```

Everything the router could not handle is left behind in the escrow. The
wrapped side is an ordinary SPL token that any AMM, aggregator or market maker
routes without a special case.

`process_wrap` nets out a source-side `TransferFee` **before** pricing the
deposit, so an underlying that taxes its own transfers cannot silently
under-fund the escrow — the reserve accounting stays exact even when the thing
being wrapped is hostile to accounting.

Going the other way (`SPL -> T22`) is where the yield lives: the wrapped mint
carries `TransferFeeConfig`, and that fee is what drives NAV. Pick the
direction that matches the job.

## Instruction encoding

No IDL, no Borsh — a single-byte discriminant.

```
0  CreateMint
1  Wrap        [u8 tag][u64 amount LE][u8 bump]     (9 accounts — the program
                                                    CPIs TransferChecked, then
                                                    mints. No separate deposit.)
2  Unwrap      [u8 tag][u64 shares LE][u8 bump]     (9 accounts — account 8 is
                                                    the unwrapped token program;
                                                    the HOLDER signs the burn,
                                                    the PDA signs the release)
3  CrankFees   [u8 tag][u8 bump]
```

Client wrap copy for `GET https://x402.accrue.fund/supported` lives in
`supported/solana-spl-token-wrap.json`. Every `spl-token-wrap` row is a single
9-account Wrap. Merging this repo does **not** publish that file to the
facilitator — copy those `acquire.steps` into the worker that serves
`/supported` and redeploy it.

## Status

Deployed to Solana mainnet and devnet at
`FrSERTNCPvTtaDS9AvQp9u1nYGzXDb3kC9MdL8Xxn2NE` (upgradeable).

**13 host tests pass, and `e2e/e2e.mjs` runs end-to-end against a real
validator** — wrap, donate, unwrap, asserting NAV moved. Green on devnet and
mainnet.

That test earned its keep immediately: `unwrap` burned from the holder's token
account while signing with the program's PDA, and a token account can only be
debited by its **owner**. Every unwrap would have failed. The host tests all
passed against that build, because the arithmetic is identical either way and
only a validator enforces account ownership.

The crank's harvest path is wired but has **not** been exercised end-to-end —
no test has yet collected withheld fees and watched NAV rise from them.

Unaudited.

## Build

```sh
cargo test                                            # host tests
cargo-build-sbf --manifest-path pinocchio-program/Cargo.toml
```

Requires a recent `cargo-build-sbf` (4.x / platform-tools ≥1.54); older
toolchains fail to resolve dependencies.
