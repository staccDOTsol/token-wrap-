/**
 * Client-copy contract for the 9-account Wrap.
 *
 * The on-chain program (slot 440219442) CPIs the deposit. A builder that
 * still emits TransferChecked-then-5-account-Wrap is what has been breaking
 * every x402 wrap since 2026-08-18.
 */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { PublicKey } from "@solana/web3.js";
import {
  WRAP_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  TOKEN_2022_PROGRAM_ID,
  WRAP_TAG,
  UNWRAP_TAG,
  buildWrapInstruction,
  buildUnwrapInstruction,
  isTransferChecked,
  hasTransferCheckedAfterWrap,
} from "./wrap.mjs";

const PK = (s) => new PublicKey(s);

const SAMPLE = {
  escrow: PK("2qLm8aCvn6gQVUFeQ7EC5J62Y95gFzc3vReHzD5d5Gj2"),
  wrappedMint: PK("6ZjjxcoicqM4nniddkuPVwew4PDwY3swbfHsGbCuLuTv"),
  recipientWrappedAta: PK("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"),
  authority: PK("EBGYMEEEPKu7szPUbnbp2h63azY9Sj9GR4MA2Ms6Quoi"),
  wrappedTokenProgram: TOKEN_2022_PROGRAM_ID,
  depositorUnderlyingAta: PK("2ZFYUDiYbtJ8czCPnd6Wjbeo1Yg1LLJ9JkGPMeuZkKyh"),
  depositor: PK("WzMaL78srutrF6CsxEkWuhMaDF5HZA6jNRaEPengqpb"),
  unwrappedMint: PK("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"),
  unwrappedTokenProgram: TOKEN_PROGRAM_ID,
  amount: 1_000_000,
  bump: 253,
};

test("wrap ix.keys.length === 9", () => {
  const ix = buildWrapInstruction(SAMPLE);
  assert.equal(ix.keys.length, 9);
  assert.ok(ix.programId.equals(WRAP_PROGRAM_ID));
  assert.equal(ix.data[0], WRAP_TAG);
  assert.equal(ix.data.length, 10);
});

test("wrap account 4 is wrapped token program, account 8 is unwrapped", () => {
  const ix = buildWrapInstruction(SAMPLE);
  assert.ok(ix.keys[4].pubkey.equals(TOKEN_2022_PROGRAM_ID));
  assert.ok(ix.keys[8].pubkey.equals(TOKEN_PROGRAM_ID));
  assert.equal(ix.keys[4].isWritable, false);
  assert.equal(ix.keys[8].isWritable, false);
  assert.equal(ix.keys[5].isWritable, true);
  assert.equal(ix.keys[6].isSigner, true);
});

test("wrap builder does not emit TransferChecked", () => {
  const ix = buildWrapInstruction(SAMPLE);
  assert.equal(isTransferChecked(ix), false);
  assert.equal(hasTransferCheckedAfterWrap([ix]), false);
});

test("no TransferChecked after Wrap in a client tx", () => {
  const wrap = buildWrapInstruction(SAMPLE);
  assert.equal(hasTransferCheckedAfterWrap([wrap]), false);

  // The banned historical shape: TransferChecked then Wrap.
  const bannedTransfer = {
    programId: TOKEN_PROGRAM_ID,
    data: Buffer.from([12, 0, 0, 0, 0, 0, 0, 0, 0, 6]),
    keys: [],
  };
  assert.equal(hasTransferCheckedAfterWrap([bannedTransfer, wrap]), false);
  assert.equal(hasTransferCheckedAfterWrap([wrap, bannedTransfer]), true);
});

test("unwrap ix.keys.length === 9 and account 8 is unwrapped token program", () => {
  const ix = buildUnwrapInstruction({
    escrow: SAMPLE.escrow,
    wrappedMint: SAMPLE.wrappedMint,
    holderWrappedAta: SAMPLE.recipientWrappedAta,
    holder: SAMPLE.depositor,
    recipientUnwrappedAta: SAMPLE.depositorUnderlyingAta,
    authority: SAMPLE.authority,
    unwrappedMint: SAMPLE.unwrappedMint,
    wrappedTokenProgram: TOKEN_2022_PROGRAM_ID,
    unwrappedTokenProgram: TOKEN_PROGRAM_ID,
    shares: 500_000,
    bump: 253,
  });
  assert.equal(ix.keys.length, 9);
  assert.equal(ix.data[0], UNWRAP_TAG);
  assert.ok(ix.keys[7].pubkey.equals(TOKEN_2022_PROGRAM_ID));
  assert.ok(ix.keys[8].pubkey.equals(TOKEN_PROGRAM_ID));
});

test("every supported spl-token-wrap row is Wrap-only (no TransferChecked)", () => {
  const kinds = JSON.parse(
    readFileSync(
      new URL("../supported/solana-spl-token-wrap.json", import.meta.url),
      "utf8",
    ),
  );
  assert.ok(Array.isArray(kinds) && kinds.length >= 3);
  for (const row of kinds) {
    const acq = row?.extra?.acquire;
    assert.equal(acq?.method, "spl-token-wrap");
    assert.ok(Array.isArray(acq.steps), `${row.extra.symbol} missing steps`);
    assert.equal(
      acq.steps.length,
      1,
      `${row.extra.symbol} must be a single Wrap step, got ${acq.steps.length}`,
    );
    assert.equal(acq.steps[0].instruction, "Wrap");
    assert.ok(
      !acq.steps.some((s) => s.instruction === "TransferChecked"),
      `${row.extra.symbol} still lists TransferChecked`,
    );
    assert.match(
      acq.steps[0].note,
      /NINE accounts/i,
      `${row.extra.symbol} note must describe the 9-account Wrap`,
    );
    assert.match(acq.steps[0].note, /program pulls the deposit itself/i);
  }
});
