/**
 * Create the yield-bearing USDC wrapper on Solana.
 *
 * The wrapped mint carries `TransferFeeConfig` and nothing else, with BOTH fee
 * authorities set to the program's mint-authority PDA. That is not incidental:
 *
 *   - the mint authority must be the PDA or the program cannot mint/burn shares
 *   - the WITHDRAW WITHHELD authority must be the PDA or the crank cannot
 *     complete — harvest is permissionless but withdraw is authority-gated, so
 *     a keypair there stalls the yield whenever its holder is offline
 *
 * Deliberately no freeze authority: a settlement asset whose transfers can be
 * frozen is not spendable without permission, which defeats the point.
 */
import {
  Connection, Keypair, PublicKey, SystemProgram, Transaction, sendAndConfirmTransaction,
} from "@solana/web3.js";
import {
  TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID, ExtensionType, getMintLen,
  createInitializeMintInstruction, createInitializeTransferFeeConfigInstruction,
  getAssociatedTokenAddressSync, createAssociatedTokenAccountInstruction,
} from "@solana/spl-token";
import { readFileSync } from "node:fs";

const NET = process.argv[2] ?? "mainnet";
const URL = NET === "mainnet"
  ? "https://api.mainnet-beta.solana.com" : "https://api.devnet.solana.com";
const PROGRAM = new PublicKey("FrSERTNCPvTtaDS9AvQp9u1nYGzXDb3kC9MdL8Xxn2NE");
// Circle USDC on Solana mainnet.
const USDC = new PublicKey(
  NET === "mainnet" ? "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
                    : "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU"
);
const TREASURY = new PublicKey("WzMaL78srutrF6CsxEkWuhMaDF5HZA6jNRaEPengqpb");

// 2bps — matches the EVM wrappers so the asset behaves identically on either
// chain rather than differing by whichever was written last.
const FEE_BPS = 20;
const MAX_FEE = BigInt("18446744073709551615"); // no cap: a maximum makes the
                                                // rate regressive and creates a
                                                // size above which it stops.

const payer = Keypair.fromSecretKey(
  Uint8Array.from(JSON.parse(readFileSync(process.env.HOME + "/jjj.json", "utf8")))
);
const conn = new Connection(URL, "confirmed");
const mint = Keypair.generate();
const [authority, bump] = PublicKey.findProgramAddressSync(
  [Buffer.from("mint_authority"), mint.publicKey.toBuffer()], PROGRAM
);

console.log(`\nyUSDCx on ${NET}`);
console.log(`  underlying   ${USDC.toBase58()}`);
console.log(`  wrapped mint ${mint.publicKey.toBase58()}`);
console.log(`  authority    ${authority.toBase58()} (bump ${bump})`);

const len = getMintLen([ExtensionType.TransferFeeConfig]);
const rent = await conn.getMinimumBalanceForRentExemption(len);

// USDC on Solana is a LEGACY SPL Token mint, so its escrow lives under the
// legacy program. The wrapped share is Token-2022 because that is where the
// TransferFee extension is — wrapping across programs is the whole point.
const escrow = getAssociatedTokenAddressSync(USDC, authority, true, TOKEN_PROGRAM_ID);
const treasuryAta = getAssociatedTokenAddressSync(mint.publicKey, TREASURY, false, TOKEN_2022_PROGRAM_ID);
const scratch = getAssociatedTokenAddressSync(mint.publicKey, authority, true, TOKEN_2022_PROGRAM_ID);

const tx = new Transaction().add(
  SystemProgram.createAccount({
    fromPubkey: payer.publicKey, newAccountPubkey: mint.publicKey,
    space: len, lamports: rent, programId: TOKEN_2022_PROGRAM_ID,
  }),
  // Extensions BEFORE InitializeMint — Token-2022 requires it.
  createInitializeTransferFeeConfigInstruction(
    mint.publicKey, authority, authority, FEE_BPS, MAX_FEE, TOKEN_2022_PROGRAM_ID
  ),
  createInitializeMintInstruction(mint.publicKey, 6, authority, null, TOKEN_2022_PROGRAM_ID),
  createAssociatedTokenAccountInstruction(payer.publicKey, escrow, authority, USDC, TOKEN_PROGRAM_ID),
  createAssociatedTokenAccountInstruction(payer.publicKey, scratch, authority, mint.publicKey, TOKEN_2022_PROGRAM_ID),
  createAssociatedTokenAccountInstruction(payer.publicKey, treasuryAta, TREASURY, mint.publicKey, TOKEN_2022_PROGRAM_ID),
);

try {
  const sig = await sendAndConfirmTransaction(conn, tx, [payer, mint]);
  console.log(`\n  created in ${sig}`);
  console.log(`  escrow    ${escrow.toBase58()}`);
  console.log(`  scratch   ${scratch.toBase58()}`);
  console.log(`  treasury  ${treasuryAta.toBase58()}`);
  console.log(`\n  fee ${FEE_BPS}bps · both fee authorities = PDA · no freeze authority\n`);
} catch (e) {
  console.log(`\n  FAILED: ${String(e.message).slice(0, 300)}`);
  (e.logs ?? []).slice(-6).forEach((l) => console.log(`    ${l}`));
  process.exit(1);
}
