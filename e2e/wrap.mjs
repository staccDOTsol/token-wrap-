/**
 * Client builders for the live wrap-nav program
 * `FrSERTNCPvTtaDS9AvQp9u1nYGzXDb3kC9MdL8Xxn2NE`.
 *
 * Slot 440219442 made Wrap pull the deposit itself. A 5-account Wrap (or a
 * TransferChecked followed by a 5-account Wrap) dies `NotEnoughAccounts`
 * (0x6a). Account 4 is the wrapped token program; account 8 is the unwrapped
 * token program — they are not interchangeable (wLEOSx: Token-2022 shares,
 * Tokenkeg escrow).
 */
import { PublicKey, TransactionInstruction } from "@solana/web3.js";

export const WRAP_PROGRAM_ID = new PublicKey(
  "FrSERTNCPvTtaDS9AvQp9u1nYGzXDb3kC9MdL8Xxn2NE",
);
export const AUTHORITY_SEED = Buffer.from("mint_authority");

export const TOKEN_PROGRAM_ID = new PublicKey(
  "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
);
export const TOKEN_2022_PROGRAM_ID = new PublicKey(
  "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb",
);

/** Token program TransferChecked discriminant. */
export const TRANSFER_CHECKED = 12;
/** wrap-nav Wrap discriminant. */
export const WRAP_TAG = 1;
/** wrap-nav Unwrap discriminant. */
export const UNWRAP_TAG = 2;

export function u64le(n) {
  const b = Buffer.alloc(8);
  b.writeBigUInt64LE(BigInt(n));
  return b;
}

/**
 * One Wrap instruction. Nine accounts, no companion TransferChecked.
 *
 * data = [1] ++ u64 amount LE ++ [bump]
 */
export function buildWrapInstruction({
  programId = WRAP_PROGRAM_ID,
  escrow,
  wrappedMint,
  recipientWrappedAta,
  authority,
  wrappedTokenProgram,
  depositorUnderlyingAta,
  depositor,
  unwrappedMint,
  unwrappedTokenProgram,
  amount,
  bump,
}) {
  return new TransactionInstruction({
    programId,
    keys: [
      { pubkey: escrow, isSigner: false, isWritable: true },
      { pubkey: wrappedMint, isSigner: false, isWritable: true },
      { pubkey: recipientWrappedAta, isSigner: false, isWritable: true },
      { pubkey: authority, isSigner: false, isWritable: false },
      { pubkey: wrappedTokenProgram, isSigner: false, isWritable: false },
      { pubkey: depositorUnderlyingAta, isSigner: false, isWritable: true },
      { pubkey: depositor, isSigner: true, isWritable: false },
      { pubkey: unwrappedMint, isSigner: false, isWritable: false },
      { pubkey: unwrappedTokenProgram, isSigner: false, isWritable: false },
    ],
    data: Buffer.concat([
      Buffer.from([WRAP_TAG]),
      u64le(amount),
      Buffer.from([bump]),
    ]),
  });
}

/**
 * Unwrap. Nine accounts; account 8 is the unwrapped token program.
 *
 * data = [2] ++ u64 shares LE ++ [bump]
 */
export function buildUnwrapInstruction({
  programId = WRAP_PROGRAM_ID,
  escrow,
  wrappedMint,
  holderWrappedAta,
  holder,
  recipientUnwrappedAta,
  authority,
  unwrappedMint,
  wrappedTokenProgram,
  unwrappedTokenProgram,
  shares,
  bump,
}) {
  return new TransactionInstruction({
    programId,
    keys: [
      { pubkey: escrow, isSigner: false, isWritable: true },
      { pubkey: wrappedMint, isSigner: false, isWritable: true },
      { pubkey: holderWrappedAta, isSigner: false, isWritable: true },
      { pubkey: holder, isSigner: true, isWritable: false },
      { pubkey: recipientUnwrappedAta, isSigner: false, isWritable: true },
      { pubkey: authority, isSigner: false, isWritable: false },
      { pubkey: unwrappedMint, isSigner: false, isWritable: false },
      { pubkey: wrappedTokenProgram, isSigner: false, isWritable: false },
      { pubkey: unwrappedTokenProgram, isSigner: false, isWritable: false },
    ],
    data: Buffer.concat([
      Buffer.from([UNWRAP_TAG]),
      u64le(shares),
      Buffer.from([bump]),
    ]),
  });
}

/** True when `ix` is a token-program TransferChecked. */
export function isTransferChecked(ix) {
  const prog = ix.programId?.toBase58?.() ?? String(ix.programId ?? "");
  const isToken =
    prog === TOKEN_PROGRAM_ID.toBase58() ||
    prog === TOKEN_2022_PROGRAM_ID.toBase58();
  return isToken && ix.data?.[0] === TRANSFER_CHECKED;
}

/**
 * True when a TransferChecked appears after the Wrap instruction in `ixs`.
 * The live program rejects that client shape: the deposit is inside Wrap.
 */
export function hasTransferCheckedAfterWrap(ixs) {
  const wrapIdx = ixs.findIndex(
    (ix) =>
      (ix.programId?.equals?.(WRAP_PROGRAM_ID) ??
        ix.programId?.toBase58?.() === WRAP_PROGRAM_ID.toBase58()) &&
      ix.data?.[0] === WRAP_TAG,
  );
  if (wrapIdx < 0) return false;
  return ixs.slice(wrapIdx + 1).some(isTransferChecked);
}
