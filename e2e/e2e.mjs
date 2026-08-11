/**
 * End-to-end: create a pool, wrap, unwrap, and prove NAV moved.
 *
 * The host tests cover the arithmetic thoroughly and cover the account wiring
 * not at all. Everything that can still be wrong lives here: account order,
 * instruction encoding, PDA derivation, signer seeds. A wrong order in
 * `unwrap` burns shares and fails to release assets — which is exactly the
 * shape of bug that only a validator finds.
 *
 *   node e2e.mjs devnet     (default)
 *   node e2e.mjs mainnet
 */
import {
  Connection, Keypair, PublicKey, SystemProgram, Transaction,
  TransactionInstruction, sendAndConfirmTransaction,
} from "@solana/web3.js";
import {
  TOKEN_2022_PROGRAM_ID, createInitializeMintInstruction, getMintLen,
  createAssociatedTokenAccountInstruction, getAssociatedTokenAddressSync,
  createMintToInstruction, getAccount, getMint, ExtensionType,
} from "@solana/spl-token";
import { readFileSync } from "node:fs";

const NET = process.argv[2] ?? "devnet";
const URL = NET === "mainnet"
  ? "https://api.mainnet-beta.solana.com"
  : "https://api.devnet.solana.com";
const PROGRAM = new PublicKey("FrSERTNCPvTtaDS9AvQp9u1nYGzXDb3kC9MdL8Xxn2NE");
const AUTHORITY_SEED = Buffer.from("mint_authority");

const payer = Keypair.fromSecretKey(
  Uint8Array.from(JSON.parse(readFileSync(process.env.HOME + "/jjj.json", "utf8")))
);
const conn = new Connection(URL, "confirmed");

const ok = (c, m) => console.log(`${c ? "  ok  " : "  FAIL"} ${m}`);
let failures = 0;
const must = (c, m) => { ok(c, m); if (!c) failures++; };

// Instruction encoding — matches processor.rs exactly. Any drift here is the
// bug this test exists to catch.
const ixWrap = (accounts, amount, bump) => new TransactionInstruction({
  programId: PROGRAM, keys: accounts,
  data: Buffer.concat([Buffer.from([1]), u64le(amount), Buffer.from([bump])]),
});
const ixUnwrap = (accounts, shares, bump) => new TransactionInstruction({
  programId: PROGRAM, keys: accounts,
  data: Buffer.concat([Buffer.from([2]), u64le(shares), Buffer.from([bump])]),
});
function u64le(n) { const b = Buffer.alloc(8); b.writeBigUInt64LE(BigInt(n)); return b; }

console.log(`\nwrap-nav e2e · ${NET} · ${PROGRAM.toBase58()}`);
console.log(`payer ${payer.publicKey.toBase58()}\n`);

// ── 1. An underlying mint we control, standing in for USDC.
const underlying = Keypair.generate();
const mintLen = getMintLen([]);
const rent = await conn.getMinimumBalanceForRentExemption(mintLen);
let tx = new Transaction().add(
  SystemProgram.createAccount({
    fromPubkey: payer.publicKey, newAccountPubkey: underlying.publicKey,
    space: mintLen, lamports: rent, programId: TOKEN_2022_PROGRAM_ID,
  }),
  createInitializeMintInstruction(underlying.publicKey, 6, payer.publicKey, null, TOKEN_2022_PROGRAM_ID),
);
await sendAndConfirmTransaction(conn, tx, [payer, underlying]);
console.log(`  underlying mint ${underlying.publicKey.toBase58()}`);

// ── 2. The wrapped mint, with the PDA as mint authority.
const wrapped = Keypair.generate();
const [authority, bump] = PublicKey.findProgramAddressSync(
  [AUTHORITY_SEED, wrapped.publicKey.toBuffer()], PROGRAM
);
tx = new Transaction().add(
  SystemProgram.createAccount({
    fromPubkey: payer.publicKey, newAccountPubkey: wrapped.publicKey,
    space: mintLen, lamports: rent, programId: TOKEN_2022_PROGRAM_ID,
  }),
  // Authority is the PDA: the program must be able to mint and burn shares.
  createInitializeMintInstruction(wrapped.publicKey, 6, authority, null, TOKEN_2022_PROGRAM_ID),
);
await sendAndConfirmTransaction(conn, tx, [payer, wrapped]);
console.log(`  wrapped mint    ${wrapped.publicKey.toBase58()}  (authority PDA bump ${bump})`);

// ── 3. Escrow (owned by the PDA) + user accounts.
const escrow = getAssociatedTokenAddressSync(underlying.publicKey, authority, true, TOKEN_2022_PROGRAM_ID);
const userUnderlying = getAssociatedTokenAddressSync(underlying.publicKey, payer.publicKey, false, TOKEN_2022_PROGRAM_ID);
const userWrapped = getAssociatedTokenAddressSync(wrapped.publicKey, payer.publicKey, false, TOKEN_2022_PROGRAM_ID);
tx = new Transaction().add(
  createAssociatedTokenAccountInstruction(payer.publicKey, escrow, authority, underlying.publicKey, TOKEN_2022_PROGRAM_ID),
  createAssociatedTokenAccountInstruction(payer.publicKey, userUnderlying, payer.publicKey, underlying.publicKey, TOKEN_2022_PROGRAM_ID),
  createAssociatedTokenAccountInstruction(payer.publicKey, userWrapped, payer.publicKey, wrapped.publicKey, TOKEN_2022_PROGRAM_ID),
  createMintToInstruction(underlying.publicKey, userUnderlying, payer.publicKey, 1_000_000, [], TOKEN_2022_PROGRAM_ID),
);
await sendAndConfirmTransaction(conn, tx, [payer]);
console.log(`  escrow          ${escrow.toBase58()}\n`);

// ── 4. Seed the escrow, then wrap. The program reads reserves from the escrow
// balance, so the deposit transfer is the caller's job in the same tx.
const DEPOSIT = 100_000;
const { createTransferCheckedInstruction } = await import("@solana/spl-token");
const wrapAccounts = [
  { pubkey: escrow, isSigner: false, isWritable: true },
  { pubkey: wrapped.publicKey, isSigner: false, isWritable: true },
  { pubkey: userWrapped, isSigner: false, isWritable: true },
  { pubkey: authority, isSigner: false, isWritable: false },
  { pubkey: TOKEN_2022_PROGRAM_ID, isSigner: false, isWritable: false },
];
tx = new Transaction().add(
  createTransferCheckedInstruction(userUnderlying, underlying.publicKey, escrow, payer.publicKey, DEPOSIT, 6, [], TOKEN_2022_PROGRAM_ID),
  ixWrap(wrapAccounts, DEPOSIT, bump),
);
try {
  const sig = await sendAndConfirmTransaction(conn, tx, [payer]);
  console.log(`  wrap tx ${sig.slice(0, 24)}…`);
} catch (e) {
  console.log(`  wrap FAILED: ${String(e.message).slice(0, 300)}`);
  const logs = e.logs ?? (await e.getLogs?.(conn)) ?? [];
  logs.slice(-8).forEach((l) => console.log(`      ${l}`));
  process.exit(1);
}

const shares = Number((await getAccount(conn, userWrapped, "confirmed", TOKEN_2022_PROGRAM_ID)).amount);
const reserves = Number((await getAccount(conn, escrow, "confirmed", TOKEN_2022_PROGRAM_ID)).amount);
const supply = Number((await getMint(conn, wrapped.publicKey, "confirmed", TOKEN_2022_PROGRAM_ID)).supply);
console.log(`\n  after wrap: reserves ${reserves} · supply ${supply} · your shares ${shares}`);
// First deposit locks MINIMUM_LIQUIDITY (1000) and is otherwise 1:1.
must(shares === DEPOSIT - 1000, `first deposit mints ${DEPOSIT - 1000} (locks MINIMUM_LIQUIDITY)`);
must(reserves === DEPOSIT, "escrow holds the full deposit");

// ── 5. Donate to the escrow WITHOUT minting. This is the yield event: NAV
// must rise for existing holders.
const DONATE = 50_000;
tx = new Transaction().add(
  createTransferCheckedInstruction(userUnderlying, underlying.publicKey, escrow, payer.publicKey, DONATE, 6, [], TOKEN_2022_PROGRAM_ID),
);
await sendAndConfirmTransaction(conn, tx, [payer]);
const reserves2 = Number((await getAccount(conn, escrow, "confirmed", TOKEN_2022_PROGRAM_ID)).amount);
console.log(`\n  donated ${DONATE} to escrow — reserves now ${reserves2}, supply unchanged ${supply}`);

// ── 6. Unwrap half. Must return MORE than face value, because NAV rose.
const REDEEM = Math.floor(shares / 2);
const unwrapAccounts = [
  { pubkey: escrow, isSigner: false, isWritable: true },
  { pubkey: wrapped.publicKey, isSigner: false, isWritable: true },
  { pubkey: userWrapped, isSigner: false, isWritable: true },
  // The HOLDER signs the burn — a token account can only be debited by its
  // owner. The PDA below signs only the escrow release.
  { pubkey: payer.publicKey, isSigner: true, isWritable: false },
  { pubkey: userUnderlying, isSigner: false, isWritable: true },
  { pubkey: authority, isSigner: false, isWritable: false },
  { pubkey: underlying.publicKey, isSigner: false, isWritable: false },
  { pubkey: TOKEN_2022_PROGRAM_ID, isSigner: false, isWritable: false },
];
const beforeUnderlying = Number((await getAccount(conn, userUnderlying, "confirmed", TOKEN_2022_PROGRAM_ID)).amount);
try {
  const sig = await sendAndConfirmTransaction(conn, new Transaction().add(ixUnwrap(unwrapAccounts, REDEEM, bump)), [payer]);
  console.log(`  unwrap tx ${sig.slice(0, 24)}…`);
} catch (e) {
  console.log(`  unwrap FAILED: ${String(e.message).slice(0, 300)}`);
  (e.logs ?? []).slice(-8).forEach((l) => console.log(`      ${l}`));
  process.exit(1);
}
const afterUnderlying = Number((await getAccount(conn, userUnderlying, "confirmed", TOKEN_2022_PROGRAM_ID)).amount);
const got = afterUnderlying - beforeUnderlying;
const expected = Math.floor((REDEEM * reserves2) / supply);
console.log(`\n  redeemed ${REDEEM} shares -> ${got} assets (expected ${expected})`);
must(got === expected, "unwrap pays exactly shares * reserves / supply");
must(got > REDEEM, `NAV rose: ${got} assets for ${REDEEM} shares`);

// ── 7. Backpointer: the on-chain registry ────────────────────────────────
// Without this there is nothing to enumerate — the wrapped mint is a caller
// Keypair, not a PDA of the underlying, so a client cannot derive a market and
// has to replay history to find one. Registering makes the whole set readable
// with a single getProgramAccounts on dataSize.
const BACKPOINTER_SEED = Buffer.from("backpointer");
const BACKPOINTER_LEN = 136;
const [backpointer, bpBump] = PublicKey.findProgramAddressSync(
  [BACKPOINTER_SEED, wrapped.publicKey.toBuffer()], PROGRAM,
);
console.log(`\n  backpointer     ${backpointer.toBase58()}  (bump ${bpBump})`);

// Pre-fund the PDA. The program does Allocate+Assign signed by the PDA rather
// than CreateAccount, so it never needs the rent sysvar or a lamport figure.
const bpRent = await conn.getMinimumBalanceForRentExemption(BACKPOINTER_LEN);
await sendAndConfirmTransaction(conn, new Transaction().add(
  SystemProgram.transfer({ fromPubkey: payer.publicKey, toPubkey: backpointer, lamports: bpRent }),
), [payer]);

const ixInitBp = (bump) => new TransactionInstruction({
  programId: PROGRAM,
  keys: [
    { pubkey: backpointer, isSigner: false, isWritable: true },
    { pubkey: wrapped.publicKey, isSigner: false, isWritable: false },
    { pubkey: escrow, isSigner: false, isWritable: false },
    { pubkey: underlying.publicKey, isSigner: false, isWritable: false },
    { pubkey: TOKEN_2022_PROGRAM_ID, isSigner: false, isWritable: false },
    { pubkey: TOKEN_2022_PROGRAM_ID, isSigner: false, isWritable: false },
    // The CPI target must be in the account list or the runtime cannot resolve
    // it — "Unknown program 111…111" and a missing-account failure.
    { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
  ],
  data: Buffer.concat([Buffer.from([4]), Buffer.from([bump])]),
});

try {
  const sig = await sendAndConfirmTransaction(conn, new Transaction().add(ixInitBp(bpBump)), [payer]);
  console.log(`  init_backpointer tx ${sig.slice(0, 24)}…`);
} catch (e) {
  console.log(`  init_backpointer FAILED: ${String(e.message).slice(0, 300)}`);
  (e.logs ?? []).slice(-10).forEach((l) => console.log(`      ${l}`));
  process.exit(1);
}

const bpInfo = await conn.getAccountInfo(backpointer, "confirmed");
must(!!bpInfo, "backpointer account exists");
must(bpInfo?.owner.equals(PROGRAM), "backpointer is owned by the program");
must(bpInfo?.data.length === BACKPOINTER_LEN, `backpointer is ${BACKPOINTER_LEN} bytes`);
if (bpInfo) {
  const d = bpInfo.data;
  const rdUnwrapped = new PublicKey(d.subarray(0, 32));
  const rdEscrow = new PublicKey(d.subarray(32, 64));
  const rdUnwrappedProg = new PublicKey(d.subarray(64, 96));
  // Bytes 0..32 are byte-identical to upstream's Backpointer, so upstream
  // readers keep working against records this program writes.
  must(rdUnwrapped.equals(underlying.publicKey), "bytes 0..32 = unwrapped mint (upstream-compatible)");
  must(rdEscrow.equals(escrow), "bytes 32..64 = escrow (reserves account)");
  must(rdUnwrappedProg.equals(TOKEN_2022_PROGRAM_ID), "bytes 64..96 = unwrapped token program");
}

// Idempotent: a second caller racing to register the same pair must succeed,
// not fail, or indexers fight each other.
try {
  await sendAndConfirmTransaction(conn, new Transaction().add(ixInitBp(bpBump)), [payer]);
  ok(true, "re-registering the same pair is a no-op (idempotent)");
} catch (e) {
  must(false, `re-register should be idempotent, got: ${String(e.message).slice(0, 120)}`);
}

// The registry is enumerable — this is the entire point.
const found = await conn.getProgramAccounts(PROGRAM, {
  commitment: "confirmed",
  filters: [{ dataSize: BACKPOINTER_LEN }],
});
must(found.some((f) => f.pubkey.equals(backpointer)),
  `getProgramAccounts(dataSize=${BACKPOINTER_LEN}) finds this market (${found.length} total)`);

console.log(failures === 0 ? "\nALL PASS\n" : `\n${failures} FAILED\n`);
process.exit(failures ? 1 : 0);
