# GraviFi

**Every market, in one orbit.** Index baskets of tokenized stocks, crypto and memecoins on
Solana, held as one token in your own wallet.

This repository is the **GraviFi vault program**, the on-chain code that holds every basket's
tokens, mints and redeems its shares, rebalances it and runs GRAVI's buyback and burn, with its
test suite. How it all works is in the [whitepaper](WHITEPAPER.md)
([PDF](GraviFi-Whitepaper.pdf)).

| | |
|---|---|
| App | [app.gravifi.xyz](https://app.gravifi.xyz) |
| Site | [gravifi.xyz](https://gravifi.xyz) |
| Program | `HHmD4ZP5he9GrcmcNrpjahyK1dWh2MhMwmVpiPRJsw3G` |
| GRAVI | `TRGNtERnbRtVkaMJ1MqU4DzSEkHrtd2qFesh6Rgravi` |
| X | [@gravifiXYZ](https://x.com/gravifiXYZ) |

## What the program guarantees

- **Redemption is always open.** No setting, creator, delisting or authority can stop a holder
  from swapping shares back for the tokens they represent.
- **Every share is fully backed.** Deposits round up, payouts round down, and value leaves a
  vault only through a redemption, a checked rebalance slice or the buyback.
- **Fee limits are code.** The protocol fee can't exceed 0.50% per mint or redeem, a creator
  fee can't exceed 0.20% a year, and a change to a creator fee never applies to time already
  passed.
- **The burn address has no key.** At least half of every protocol fee goes to it, a floor in
  the code, and it can only be spent buying GRAVI and burning it.

## Layout

```
programs/gravifi-vault/        the vault program (Rust, Anchor 0.31.1)
  src/lib.rs                   the index of every instruction
  src/instructions/            their code and accounts, by area: protocol, basket,
                               mint_redeem, upkeep, rebalance, buyback
  src/state.rs                 the accounts, and the limits the program enforces
tests/                         the test suite, run against the compiled program in LiteSVM
WHITEPAPER.md                  how GraviFi works
```

## Build and test

Requires the Solana CLI 2.1 and Anchor 0.31.1.

```bash
anchor build
npm ci
npm test
```

The tests load `target/deploy/gravifi_vault.so` from the build and exercise it end to end:
baskets, minting and redemption, rounding, fees, access modes, rebalancing and its price
floor, sweeps of tokens a basket is sent, splits, Token-2022 transfer fees, and the buyback
and burn.

## Verify it's the program on mainnet

The program built from this repository is byte-for-byte the one deployed. Build it, download
the deployed one, and compare (the on-chain copy is zero-padded to its allocated size):

```bash
anchor build
solana program dump -u m HHmD4ZP5he9GrcmcNrpjahyK1dWh2MhMwmVpiPRJsw3G onchain.so
head -c "$(wc -c < target/deploy/gravifi_vault.so)" onchain.so | shasum -a 256
shasum -a 256 target/deploy/gravifi_vault.so
```

Both hashes start with `0a292b7eda8c23ad`. A reproducible verified build (`solana-verify`)
comes with the move to a multisig.

## Status

Live on Solana mainnet since 14 October 2026. The program has passed an internal security
review; an independent audit is next, followed by a multisig for the upgrade authority and a
verified build. Until then, only put in what you can afford to lose.

## Security

Found something? Write to **team@gravifi.xyz**. Please don't open a public issue for a
vulnerability.

## License

[MIT](LICENSE)
