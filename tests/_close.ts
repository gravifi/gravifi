// Closing an emptied basket: the calls, as close-basket.test.ts and community.test.ts make them.
import type { Keypair } from "@solana/web3.js";
import { PublicKey, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID, ata, pda, rpc } from "./helpers.ts";
import { feeShares, type World } from "./fixtures.ts";

type W = Pick<World, "ctx" | "basket" | "shareMint" | "shareProgram" | "mintA" | "mintB" | "vaultA" | "vaultB">;

export const acct = (pubkey: PublicKey, isWritable = true) => ({ pubkey, isSigner: false, isWritable });
export const lamports = (w: W, k: PublicKey) => BigInt(w.ctx.svm.getAccount(k)?.lamports ?? 0);
export const gone = (w: W, k: PublicKey) => (w.ctx.svm.getAccount(k)?.data.length ?? 0) === 0 && lamports(w, k) === 0n;

/** `[vault, manager's account, mint, token program]` per asset, as close_vaults takes them. */
export const vaultQuads = (w: W, to: PublicKey) => [
  acct(w.vaultA), acct(ata(w.mintA, to, TOKEN_PROGRAM_ID)), acct(w.mintA, false), acct(TOKEN_PROGRAM_ID, false),
  acct(w.vaultB), acct(ata(w.mintB, to, TOKEN_2022_PROGRAM_ID)), acct(w.mintB, false), acct(TOKEN_2022_PROGRAM_ID, false),
];

export const closeVaults = (w: W, s: Keypair, quads = vaultQuads(w, s.publicKey)) =>
  rpc(w.ctx, w.ctx.vault.methods.closeVaults().accountsPartial({ manager: s.publicKey, basket: w.basket, shareMint: w.shareMint }).remainingAccounts(quads), [s]);

export const closeBasket = (w: W, s: Keypair, side: PublicKey[] = [], fees = true) =>
  rpc(
    w.ctx,
    w.ctx.vault.methods
      .closeBasket()
      .accountsPartial({
        manager: s.publicKey,
        basket: w.basket,
        shareMint: w.shareMint,
        protocol: pda.protocol(),
        burnAuthority: pda.burn(),
        treasuryShares: fees ? feeShares(w.shareMint, w.shareProgram).treasuryShares : null,
        burnShares: fees ? feeShares(w.shareMint, w.shareProgram).burnShares : null,
        shareTokenProgram: w.shareProgram,
      })
      .remainingAccounts([acct(w.vaultA, false), acct(w.vaultB, false), ...side.map((k) => acct(k))]),
    [s],
  );

