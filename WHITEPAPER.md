# GraviFi

**Every market, in one orbit.**

Whitepaper · October 2026 · [gravifi.xyz](https://gravifi.xyz) · [app.gravifi.xyz](https://app.gravifi.xyz) · [@gravifiXYZ](https://x.com/gravifiXYZ)

---

## Abstract

GraviFi turns a mix of tokenized stocks, crypto and memecoins into a single token. Each
**basket** is a vault on Solana that holds its tokens and issues one share token for them. A
share is always worth its exact slice of what the vault holds, and can always be swapped back
for that slice. Anyone can buy a basket with USDC or SOL, or build one from 133 listed assets.
Every basket mint and redeem pays a 0.40% protocol fee; half of it buys **GRAVI** on the open
market and burns it.

GraviFi has been live on Solana mainnet since 14 October 2026.

---

## 1. The problem

Tokenized stocks, pre-IPO tokens, crypto and memecoins now trade side by side on Solana, but
holding a spread of them is still manual: a dozen swaps to buy in, a dozen to rebalance, a
dozen to get out, and a wallet full of positions to track. Traditional index funds solve this
off-chain, behind a broker, a custodian and a fund manager, and they can't hold a memecoin or
a token that isn't listed on an exchange.

GraviFi does it on-chain: one token for the whole spread, held in your own wallet, with the
rules written into the program rather than into a prospectus.

## 2. Baskets

A basket is three things:

- **A vault.** The GraviFi program holds the basket's tokens in accounts only it controls. No
  person, not the creator and not the team, can move them outside the rules below.
- **A share token.** One token for the whole basket. It lives in the holder's wallet like any
  other Solana token and can be sent or traded freely.
- **A recipe.** The basket records how many units of each token back one share. A share's
  value (its NAV) is those units priced live.

### 2.1 Minting and redeeming

**Minting** deposits the exact underlying tokens for a number of shares and receives those
shares, less the protocol fee. **Redeeming** hands shares back and receives each token's
exact slice, also less the fee. No price is involved in either: the vault swaps units for
units, so a share can never be issued for less than it is worth, or paid out for more.

Both are priced from what the vault holds at that moment, not from a figure stored earlier,
so tokens that reach a vault any other way (an airdrop, a donation, a token a redeemer left
behind) belong to the holders already there: nobody can mint into them at the old price.

The vault always rounds in its own favour: deposits round up, payouts round down. Every
token a share promises is in the vault.

### 2.2 Redemption is always open

Redeeming is never gated: not by a basket's access mode, not by its creator, not by a
delisting, not by the team. If the issuer of one token pauses or freezes it, a holder can
leave that one token behind and take everything else; what they leave stays with the
remaining holders. **A holder who cannot redeem holds a promise, not a claim**, so GraviFi
is built so that this never happens.

### 2.3 Buying and selling with USDC or SOL

Nobody needs to hold the underlying tokens first. The app turns a payment into a mint:

1. **Swaps.** The payment is split by each token's weight and swapped through Jupiter into the
   buyer's own wallet. Each swap carries a price limit.
2. **Mint.** The vault takes what the swaps delivered and mints the shares it covers. If a swap
   misses its limit, nothing is minted, and the app offers to swap back what landed.

Selling runs the other way: redeem, then swap each token back to USDC or SOL. Or redeem and
keep the tokens. Every step is approved in the buyer's wallet, and the full cost is shown
before signing.

## 3. Creating a basket

Anyone can create a basket in the app:

| Choice | Options |
|---|---|
| **Assets** | 2 to 14, from the 133 listed |
| **Weights** | Any split adding up to 100% |
| **Rebalancing** | On a schedule, when it drifts past a tolerance, or both (at least a day apart, at least 0.5% drift) |
| **Creator fee** | 0% to 0.20% a year |
| **Access** | Public, or private (only the creator can mint; anyone holding shares can always redeem) |
| **Control** | Creator-managed, or immutable: locked forever, nobody can change it again |
| **Starting price** | $1 to $10,000 a share |

The creator seeds the basket with at least $20, and its shares go live on the app's explore
page. A creator-managed basket can later change its weights, add or remove assets, change its
rebalancing policy and fee within the caps, and split its shares.

**Splits.** A creator can split or reverse-split a basket's shares (1:100 to 100:1, at most
once every 30 days). Every holder's balance and the price per share change together in one
transaction; nobody's slice changes.

**The creator fee** is streamed as new shares, so a creator earns more only if the basket
grows. It is the only cost of holding: the protocol charges nothing while shares are held. A
change to it counts from the moment it's made, never for the time before.

## 4. Rebalancing

Prices move, so a basket's actual weights drift from its targets. When a basket's schedule
comes round, or it drifts past its tolerance, a keeper trades it back:

- **In slices** of at most 5% of the basket's value, so no single trade moves a thin market.
- **At unpredictable times**, so nobody can trade ahead of a known schedule.
- **Against a price floor.** Each slice must fetch at least the oracle price, less a small
  tolerance, or the whole slice reverts. The keeper is lent the tokens to sell, and the program
  checks what comes back to the vault in the same transaction.

The program checks every slice, and the share supply can't change in the middle of one. The
floor is only as good as the price it's checked against: today the keeper's price authority
posts those prices (from Jupiter), so a rebalance can't take value out of a basket as long as
the prices posted are honest. Section 8 says who holds that key, and the roadmap bounds it.

**Tokens a basket is sent.** Some tokens pay their holders in another token (a stonkfun token
can pay ZEC), and anyone can send a basket an airdrop. Those tokens aren't part of the recipe,
so the keeper sweeps them: it swaps them into the basket's most underweight token against the
same oracle floor, and their value goes to the holders.

## 5. Fees

| Fee | Rate | Who pays, when | Where it goes |
|---|---|---|---|
| **Protocol fee** | 0.40% | On every mint and redeem, including a creator's first deposit | Half to the GRAVI burn, half to the treasury |
| **Creator fee** | 0% to 0.20% a year | Holders, as share dilution while held | The basket's creator |
| **App fee** | 0.05% | On buys and sells through the app's USDC/SOL route | The treasury |
| **Network** | fractions of a cent | Every transaction | Solana validators |

The protocol fee rounds up, so no flow is too small to pay it, and it is kept in shares, so
the vault stays fully backed. The limits are in the program's code: the protocol fee can never
exceed 0.50%, a creator fee can never exceed 0.20% a year, and at least half of every protocol
fee goes to the GRAVI burn. Changing any of them would take new code, in public.

Opening a token account the first time a wallet holds an asset costs about 0.002 SOL of
Solana rent, refunded when the account is closed. Trading shares wallet to wallet, or in any
pool someone else runs, pays the protocol nothing.

## 6. GRAVI

GRAVI is GraviFi's token. It carries no vote and no claim on baskets; nothing to lock, nothing
to stake. Its one job is to absorb the protocol's revenue through burns: the more GraviFi is
used, the less GRAVI there is.

| | |
|---|---|
| **Address** | `TRGNtERnbRtVkaMJ1MqU4DzSEkHrtd2qFesh6Rgravi` |
| **Supply** | 1,000,000,000 at launch, fixed: nobody can mint more |
| **Launch** | 14 October 2026, on pump.fun, paired with SOL |
| **Team** | 1.92% of supply (19,221,405 GRAVI), locked on Streamflow until 15 April 2027: one release, no cancel, no transfer |

### 6.1 Buyback and burn

Half of every protocol fee is set aside in the **burn address**
(`FkMEzsnij3aN9njxcjmGxi1DK43WChBtRWorvXgptUJr`). It has no private key: it is controlled by
the GraviFi program, and the program can only spend it one way.

1. The burn address redeems its basket shares for the underlying tokens, exactly as any holder
   would.
2. The keeper sells those tokens for GRAVI on the open market. The program checks the GRAVI
   received against the oracle price before accepting it.
3. All of the GRAVI received is burned in the same transaction.

Buybacks run in slices once there is enough to be worth the network fees, so small amounts
wait until they add up. Every burn is public on Solana, and the running total is shown live at
[app.gravifi.xyz/pulse](https://app.gravifi.xyz/pulse).

### 6.2 GRAVI's own trading fees

GRAVI's pump.fun creator fees go the same way: claimed into the team's GRAVI wallet, half buys
GRAVI and burns it, half goes to the treasury.

**Burn #1** (15 October 2026): 3.80 SOL of creator fees claimed; 1.90 SOL bought and burned
**10,611,685 GRAVI**, 1.06% of supply. Supply after: 989,388,314.

## 7. Assets

133 assets are listed today:

- **Tokenized stocks and ETFs** issued on Solana (xStocks and others): NVIDIA, Apple, Tesla,
  the S&P 500, the Nasdaq 100, gold and more.
- **Pre-IPO tokens**: Anthropic, OpenAI, Anduril, Polymarket and others.
- **Crypto**: SOL, bitcoin (wBTC, cbBTC), ETH, HYPE, ZEC, Jupiter, TAO and more.
- **Memecoins**: from BONK and WIF to stonkfun's tokens.

An asset is listed only if it has a working market on Solana. A token whose own rules let its
issuer take a cut of every transfer is accepted only up to 3%, and the program checks that
cap on every deposit. Tokens with an active transfer hook, which hands every transfer to
another program, are refused.

Tokenized stocks are issued by third parties, who can freeze or claw back their tokens under
their own terms; GraviFi can't prevent that, only keep redemption of everything else open
(section 2.2). Their issuers don't offer them to US persons.

## 8. Who controls what

There is no governance and no vote. Control is split so that no role can reach holders' funds:

| Role | Can | Can't |
|---|---|---|
| **Holders** | Redeem at any time, send or trade their shares | |
| **A basket's creator** | Change their basket's weights, assets, rebalancing, fee (within the caps), split | Touch the vault's tokens; anything at all once the basket is immutable |
| **The protocol authority** (the team) | List assets, set fees within the ceilings, name the keepers, and upgrade the program | Through any of its settings: move a vault's tokens, block a redemption, exceed the fee ceilings, send less than half of the protocol fee to the burn |
| **The keeper** | Post the prices rebalances and buybacks are checked against; run rebalance and buyback slices within the program's checks | Move a vault's tokens outside a slice, or accept less back than the posted price allows |
| **The burn address** | Buy GRAVI and burn it | Anything else: it has no key |

The program and protocol authority are held today by one team key, and upgrading a program
can change what it does. The keeper's price authority and rebalancer are one more key, held by
the app's server: whoever held it could post false prices and trade baskets at a loss, a slice
at a time. Holders trust the team with both keys today. That is why the next steps (section 10) are a multisig, so no single
key can upgrade it, and a verified build, so anyone can check that what runs on-chain is the
published code.

## 9. Risks

GraviFi is new software. Before using it, know that:

- **No independent audit yet.** The program has passed an internal security review and its
  test suite; an independent audit is next. Only put in what you can afford to lose.
- **Key risk.** The team holds the keys that upgrade the program and post the prices
  rebalances are checked against (section 8). Until the multisig and on-chain price limits
  land, a lost or misused key could cost basket holders money.
- **Issuer risk.** Tokenized stocks and pre-IPO tokens depend on their issuers, who can pause,
  freeze or claw back their tokens. A holder can still redeem everything else and leave such a
  token in the basket (section 2.2).
- **Market risk.** Baskets move with what they hold. Thinly traded tokens can cost more to buy
  and sell, and the app shows that cost before you sign.
- **Buys take more than one transaction.** The swaps land first, then the mint. If a swap fails,
  no shares are minted and the tokens already bought stay in your wallet.
- **GRAVI** is a token on an open market, with no rights attached. Its price can go to zero.

Nothing in this paper is investment advice, or an offer to sell GRAVI or any other asset.

## 10. Roadmap

1. **Independent security audit.**
2. **Multisig** for the program and protocol authority, and a **verified build**, so anyone can
   check that the program on-chain is exactly this code.
3. **Bounded prices** for rebalances and buybacks: limits in the program on how far and how fast
   a posted price can move, separate keys for prices and trading, and Pyth prices where feeds
   exist.
4. **Allowlisted baskets**, where a creator chooses who can mint.
5. **Baskets of up to 16 assets.**

## Addresses

| | |
|---|---|
| Program | `HHmD4ZP5he9GrcmcNrpjahyK1dWh2MhMwmVpiPRJsw3G` |
| GRAVI | `TRGNtERnbRtVkaMJ1MqU4DzSEkHrtd2qFesh6Rgravi` |
| Burn address | `FkMEzsnij3aN9njxcjmGxi1DK43WChBtRWorvXgptUJr` |
| Team GRAVI wallet | `FK9x1PgAqunjtm57DCSpWLuwFmNEArqbF9aieb2VgnL1` |
| Team GRAVI lock (Streamflow) | `GxtRDjGXxHQTMRh6FX1qte7yvhAP2tTkHKZrhswRhLyp` |
| Treasury | `CmBPHj38TGpjGLxgAVYvEYETBM4WYqK6GtBgksgKAym2` |
| Protocol authority | `B95P86aojwMui9aKFdwMjycbML3QFh7jgJ2VftSj9YJ5` |

Every address can be checked on [Solscan](https://solscan.io).

---

*GraviFi · [gravifi.xyz](https://gravifi.xyz) · team@gravifi.xyz (contact and security reports)*
