/**
 * Crank e2e — the yield mechanism, proven end to end.
 *
 * Everything else in this repo tests NAV rising because someone DONATED to the
 * escrow. That is the easy half. The actual product claim is that NAV rises on
 * its own from Token-2022 transfer fees, and until this script existed nothing
 * had ever collected a withheld fee and watched supply fall as a result.
 *
 * The full cycle, which only works if all four pieces line up:
 *
 *   1. a transfer of the wrapped mint withholds a fee IN THE RECIPIENT account
 *      (Token-2022 does this deliberately so one fee account never becomes a
 *      write-lock bottleneck — the cost is that collecting requires visiting
 *      every holder, which is the whole reason a crank exists)
 *   2. `harvest_withheld_tokens_to_mint` sweeps recipients → mint. Permissionless.
 *   3. `withdraw_withheld_tokens_from_mint` moves mint → scratch. AUTHORITY-GATED,
 *      which is why the PDA must hold WithdrawWithheldAuthority: a keypair here
 *      would mean yield stalls whenever its holder is offline.
 *   4. the scratch balance is split — half to treasury, half BURNED. The burn
 *      drops supply while reserves are untouched, so `reserves / supply` rises
 *      for every remaining holder. That is the yield.
 *
 * Run: node e2e-crank.mjs devnet|mainnet
 */
import {
  Connection, ComputeBudgetProgram, Keypair, PublicKey, SystemProgram, Transaction,
  TransactionInstruction, sendAndConfirmTransaction,
} from "@solana/web3.js";
import {
  TOKEN_2022_PROGRAM_ID, ExtensionType, getMintLen, getAccount, getMint,
  createInitializeMintInstruction, createInitializeTransferFeeConfigInstruction,
  createAssociatedTokenAccountInstruction, getAssociatedTokenAddressSync,
  createMintToInstruction, createTransferCheckedInstruction,
} from "@solana/spl-token";
import { readFileSync } from "node:fs";
import { buildWrapInstruction } from "./wrap.mjs";

const NET = process.argv[2] ?? "devnet";
const URL = NET === "mainnet"
  ? (process.env.SOLANA_RPC_URL || "https://solana.lava.build")
  : "https://api.devnet.solana.com";
const PROGRAM = new PublicKey("FrSERTNCPvTtaDS9AvQp9u1nYGzXDb3kC9MdL8Xxn2NE");
const AUTHORITY_SEED = Buffer.from("mint_authority");

const payer = Keypair.fromSecretKey(
  Uint8Array.from(JSON.parse(readFileSync(`${process.env.HOME}/jjj.json`, "utf8"))),
);
const conn = new Connection(URL, "confirmed");
let failures = 0;
const ok = (c, m) => console.log(`${c ? "  ok  " : "  FAIL"} ${m}`);
const must = (c, m) => { ok(c, m); if (!c) failures++; };
const u64le = (n) => { const b = Buffer.alloc(8); b.writeBigUInt64LE(BigInt(n)); return b; };

// Mainnet drops fee-less transactions during congestion — the first run of this
// script expired at block height on two different providers while an otherwise
// identical e2e had landed minutes earlier. Prepending a priority fee is the
// difference between "the program is broken" and "the transaction never got in".
const PRIORITY_MICROLAMPORTS = Number(process.env.PRIORITY_FEE ?? (NET === "mainnet" ? 200_000 : 0));
const send = async (tx, signers) => {
  // Confirm by POLLING, not by WebSocket subscription. Verified on mainnet:
  // sendAndConfirmTransaction threw TransactionExpiredBlockheightExceeded for
  // two transactions that had both LANDED (err=None, real slots) — the
  // subscription simply never delivered. Trusting it makes a healthy program
  // look broken, and worse, invites a resend of something that already
  // executed. getSignatureStatuses is the source of truth.
  if (PRIORITY_MICROLAMPORTS > 0) {
    tx = new Transaction()
      .add(ComputeBudgetProgram.setComputeUnitPrice({ microLamports: PRIORITY_MICROLAMPORTS }))
      .add(...tx.instructions);
  }
  const sig = await conn.sendTransaction(tx, signers, { maxRetries: 5 });
  for (let i = 0; i < 60; i++) {
    const st = (await conn.getSignatureStatuses([sig], { searchTransactionHistory: true })).value[0];
    if (st?.err) throw Object.assign(new Error(`tx failed: ${JSON.stringify(st.err)}`), { signature: sig });
    if (st && (st.confirmationStatus === "confirmed" || st.confirmationStatus === "finalized")) return sig;
    await new Promise((r) => setTimeout(r, 1000));
  }
  throw new Error(`tx ${sig} not confirmed after 60s`);
};

console.log(`\ncrank e2e · ${NET} · ${PROGRAM.toBase58()}\n`);

const FEE_BPS = 500;              // 5% withheld on every wrapped transfer
const MAX_FEE = 1_000_000_000n;
const DEPOSIT = 100_000;
const MOVE = 20_000;              // wrapped tokens moved holder → holder

// ── 1. underlying mint (plain T22) ───────────────────────────────────────
const underlying = Keypair.generate();
const plainLen = getMintLen([]);
const plainRent = await conn.getMinimumBalanceForRentExemption(plainLen);
await send(new Transaction().add(
  SystemProgram.createAccount({
    fromPubkey: payer.publicKey, newAccountPubkey: underlying.publicKey,
    space: plainLen, lamports: plainRent, programId: TOKEN_2022_PROGRAM_ID,
  }),
  createInitializeMintInstruction(underlying.publicKey, 6, payer.publicKey, null, TOKEN_2022_PROGRAM_ID),
), [payer, underlying]);
console.log(`  underlying      ${underlying.publicKey.toBase58()}`);

// ── 2. wrapped mint WITH TransferFeeConfig, both authorities = PDA ───────
// The transfer-fee config authority and the withdraw-withheld authority must
// BOTH be the PDA. Step 3 of the crank is authority-gated, so anything else
// makes the cycle depend on a human being online.
const wrapped = Keypair.generate();
const [authority, bump] = PublicKey.findProgramAddressSync(
  [AUTHORITY_SEED, wrapped.publicKey.toBuffer()], PROGRAM,
);
const feeLen = getMintLen([ExtensionType.TransferFeeConfig]);
const feeRent = await conn.getMinimumBalanceForRentExemption(feeLen);
await send(new Transaction().add(
  SystemProgram.createAccount({
    fromPubkey: payer.publicKey, newAccountPubkey: wrapped.publicKey,
    space: feeLen, lamports: feeRent, programId: TOKEN_2022_PROGRAM_ID,
  }),
  // Must precede InitializeMint — extensions are configured on an uninitialised mint.
  createInitializeTransferFeeConfigInstruction(
    wrapped.publicKey, authority, authority, FEE_BPS, MAX_FEE, TOKEN_2022_PROGRAM_ID,
  ),
  createInitializeMintInstruction(wrapped.publicKey, 6, authority, null, TOKEN_2022_PROGRAM_ID),
), [payer, wrapped]);
console.log(`  wrapped         ${wrapped.publicKey.toBase58()}  (fee ${FEE_BPS}bps, authority PDA bump ${bump})`);

// ── 3. accounts ──────────────────────────────────────────────────────────
const escrow = getAssociatedTokenAddressSync(underlying.publicKey, authority, true, TOKEN_2022_PROGRAM_ID);
const scratch = getAssociatedTokenAddressSync(wrapped.publicKey, authority, true, TOKEN_2022_PROGRAM_ID);
const treasury = getAssociatedTokenAddressSync(wrapped.publicKey, payer.publicKey, false, TOKEN_2022_PROGRAM_ID);
const userUnderlying = getAssociatedTokenAddressSync(underlying.publicKey, payer.publicKey, false, TOKEN_2022_PROGRAM_ID);
const holder = Keypair.generate();
const holderWrapped = getAssociatedTokenAddressSync(wrapped.publicKey, holder.publicKey, false, TOKEN_2022_PROGRAM_ID);

await send(new Transaction().add(
  createAssociatedTokenAccountInstruction(payer.publicKey, escrow, authority, underlying.publicKey, TOKEN_2022_PROGRAM_ID),
  createAssociatedTokenAccountInstruction(payer.publicKey, scratch, authority, wrapped.publicKey, TOKEN_2022_PROGRAM_ID),
  createAssociatedTokenAccountInstruction(payer.publicKey, treasury, payer.publicKey, wrapped.publicKey, TOKEN_2022_PROGRAM_ID),
  createAssociatedTokenAccountInstruction(payer.publicKey, userUnderlying, payer.publicKey, underlying.publicKey, TOKEN_2022_PROGRAM_ID),
  createAssociatedTokenAccountInstruction(payer.publicKey, holderWrapped, holder.publicKey, wrapped.publicKey, TOKEN_2022_PROGRAM_ID),
  createMintToInstruction(underlying.publicKey, userUnderlying, payer.publicKey, DEPOSIT * 2, [], TOKEN_2022_PROGRAM_ID),
), [payer]);
console.log(`  escrow          ${escrow.toBase58()}`);
console.log(`  scratch         ${scratch.toBase58()}\n`);

// ── 4. wrap only — program pulls the deposit (9 accounts) ────────────────
const treasuryWrapped = getAssociatedTokenAddressSync(wrapped.publicKey, payer.publicKey, false, TOKEN_2022_PROGRAM_ID);
await send(new Transaction().add(
  buildWrapInstruction({
    escrow,
    wrappedMint: wrapped.publicKey,
    recipientWrappedAta: treasuryWrapped,
    authority,
    wrappedTokenProgram: TOKEN_2022_PROGRAM_ID,
    depositorUnderlyingAta: userUnderlying,
    depositor: payer.publicKey,
    unwrappedMint: underlying.publicKey,
    unwrappedTokenProgram: TOKEN_2022_PROGRAM_ID,
    amount: DEPOSIT,
    bump,
  }),
), [payer]);

const reserves0 = Number((await getAccount(conn, escrow, "confirmed", TOKEN_2022_PROGRAM_ID)).amount);
const supply0 = Number((await getMint(conn, wrapped.publicKey, "confirmed", TOKEN_2022_PROGRAM_ID)).supply);
const nav0 = reserves0 / supply0;
console.log(`  after wrap: reserves ${reserves0} · supply ${supply0} · NAV ${nav0.toFixed(8)}`);

// ── 5. a transfer withholds a fee IN THE RECIPIENT ───────────────────────
await send(new Transaction().add(
  createTransferCheckedInstruction(treasuryWrapped, wrapped.publicKey, holderWrapped, payer.publicKey, MOVE, 6, [], TOKEN_2022_PROGRAM_ID),
), [payer]);
const holderAcc = await getAccount(conn, holderWrapped, "confirmed", TOKEN_2022_PROGRAM_ID);
const expectedFee = Math.floor((MOVE * FEE_BPS) / 10_000);
console.log(`\n  moved ${MOVE} wrapped -> holder received ${holderAcc.amount} (fee ${expectedFee} withheld in the RECIPIENT)`);
must(Number(holderAcc.amount) === MOVE - expectedFee, `recipient nets ${MOVE - expectedFee}`);

// ── 6. crank: harvest -> withdraw -> split/burn ──────────────────────────
const ixCrank = new TransactionInstruction({
  programId: PROGRAM,
  keys: [
    { pubkey: wrapped.publicKey, isSigner: false, isWritable: true },
    { pubkey: scratch, isSigner: false, isWritable: true },
    { pubkey: authority, isSigner: false, isWritable: false },
    { pubkey: TOKEN_2022_PROGRAM_ID, isSigner: false, isWritable: false },
    { pubkey: treasuryWrapped, isSigner: false, isWritable: true },
    // Variadic tail: every holder account to harvest from.
    { pubkey: holderWrapped, isSigner: false, isWritable: true },
  ],
  data: Buffer.concat([Buffer.from([3]), Buffer.from([bump])]),
});
const treasuryBefore = Number((await getAccount(conn, treasuryWrapped, "confirmed", TOKEN_2022_PROGRAM_ID)).amount);
try {
  const sig = await send(new Transaction().add(ixCrank), [payer]);
  console.log(`  crank tx ${sig.slice(0, 24)}…`);
} catch (e) {
  console.log(`  crank FAILED: ${String(e.message).slice(0, 300)}`);
  (e.logs ?? []).slice(-12).forEach((l) => console.log(`      ${l}`));
  process.exit(1);
}

const reserves1 = Number((await getAccount(conn, escrow, "confirmed", TOKEN_2022_PROGRAM_ID)).amount);
const supply1 = Number((await getMint(conn, wrapped.publicKey, "confirmed", TOKEN_2022_PROGRAM_ID)).supply);
const treasuryAfter = Number((await getAccount(conn, treasuryWrapped, "confirmed", TOKEN_2022_PROGRAM_ID)).amount);
const nav1 = reserves1 / supply1;

console.log(`\n  after crank: reserves ${reserves1} · supply ${supply1} · NAV ${nav1.toFixed(8)}`);
console.log(`  supply burned: ${supply0 - supply1} · treasury delta: ${treasuryAfter - treasuryBefore}`);

// The claim, stated as assertions.
must(reserves1 === reserves0, "reserves UNCHANGED — the yield is not new deposits");
must(supply1 < supply0, `supply FELL ${supply0} -> ${supply1} (fees were burned)`);
must(nav1 > nav0, `NAV ROSE ${nav0.toFixed(8)} -> ${nav1.toFixed(8)} from transfer fees alone`);
// Dev share rounds DOWN and the burn takes the remainder, so burn >= treasury
// always — a caller cranking dust can never round the treasury upward.
must((supply0 - supply1) >= (treasuryAfter - treasuryBefore),
  "burn >= treasury share (dev share rounds down)");

console.log(failures === 0 ? "\nALL PASS\n" : `\n${failures} FAILED\n`);
process.exit(failures ? 1 : 0);
