# GraviFi

**Every market, in one orbit.**

Whitepaper · November 2026 · [gravifi.xyz](https://gravifi.xyz) · [app.gravifi.xyz](https://app.gravifi.xyz) · [@gravifiXYZ](https://x.com/gravifiXYZ)

---

## Abstract

GraviFi turns a mix of tokenized stocks, crypto and memecoins into a single token. Each
**basket** is a vault on Solana that holds its tokens and issues one share token for them. A
share is always worth its exact slice of what the vault holds, and can always be swapped back
for that slice. Anyone can buy a basket with USDC or SOL, or build one from 142 GraviFi Verified assets or any
other Solana token that passes the listing check. Every basket mint and redeem pays a 0.40%
protocol fee (0.80% on a Smart basket); half of it buys **GRAVI** on the open
market and burns it. Creating a basket burns GRAVI too. GRAVI stakers share 10% of the protocol
fee, paid in SOL, and each 5,000,000 GRAVI staked makes one basket a day without burning. A basket's
holders can also run it themselves, by vote, and a Smart basket is reviewed every day within
limits the program enforces.

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

### 2.4 Recurring plans, stop losses and take profits

A holder can set up recurring buys or sells of a basket (daily to monthly, up to $10,000 a
plan), and stop losses and take profits on any basket or token they hold: fixed or trailing,
one or two levels on each side, sold at the market as soon as a level is reached, with
confirmation at once or after 1, 5 or 15 minutes. These are the holder's own exits, not the
creator's stops on a basket's assets (section 4.1).

Each plan and each stop has a wallet of its own, which the holder owns and which holds exactly
what it spends or protects. GraviFi can sign for it only through two keys together, the app's
and a separate guard's, and the guard co-signs only a fill that is due or a sale of that stop's
holding, paid back to the owner's main wallet. The owner's main wallet never gets a signer, and
cancelling sends what's left back to it. These have no fee of their own.

## 3. Creating a basket

Anyone can create a basket in the app:

| Choice | Options |
|---|---|
| **Assets** | 2 to 14, from the 142 GraviFi Verified or any token that passes the listing check (section 7.1) |
| **Weights** | Any split adding up to 100% |
| **Rebalancing** | On a schedule, when it drifts past a tolerance, or both (at least a day apart, at least 0.5% drift). Rebalancing starts once the basket holds $50 (section 4) |
| **Creator's cut** | 0% to 0.60% of each buy |
| **Access** | Public; allowlist (only wallets the creator approves can mint); or private (only the creator can mint). Anyone holding shares can always redeem |
| **Run by** | Its creator; Smart (section 3.1); its holders (section 8.1, public baskets only); or nobody: immutable, locked forever |
| **Stop losses** | Optional, per asset (section 4.1) |
| **Starting price** | $1 to $10,000 a share |

Creating the basket burns the GRAVI the protocol asks for (section 6.3); the creator then seeds
it with at least $20, and its shares go live on the app's explore page. A creator-managed basket can later
change its weights, add or remove assets, change its rebalancing policy, stop losses and cut
within the caps, and split its shares.

**Splits.** A creator can split or reverse-split a basket's shares (1:100 to 100:1, at most
once every 30 days). Every holder's balance and the price per share change together in one
transaction; nobody's slice changes.

**The creator's cut** is a share of each buy by someone else, paid in shares. Selling and holding
don't pay it, so nobody already in can be charged more by a later rise. A creator can lower it
at any time. Raising it takes a day's notice once the basket has shares, shown on its page, so no
buyer pays more than they were quoted. An immutable basket's is fixed at its first deposit.

**The yearly fee** is how creators earned before the cut: up to 0.20% a year, streamed as new
shares while held. Baskets that have one keep it until their creator chooses a cut, which stops
it. A basket has one or the other, never both, and new baskets are only offered the cut.

### 3.1 Smart baskets

A creator can make a basket Smart when creating it. The creator writes its rules (its mandate,
whose hash is stored on chain), and the app reviews the basket against them every day (four
times a day for a fast-moving "pump" mandate, weekly after 30 days with no change): a review may
add, remove and reweight assets and set the mandate's stops. The changes are made by the app's
strategist key, which can do nothing else: not move a token, not change a fee or a limit, not
touch another basket.

The program holds every Smart basket to limits: at most 40% of the targets changed in any 24
hours and 100% in any 7 days, at most 35% in one asset and 30% in one that isn't GraviFi
Verified (the protocol authority can move them within bounds in the code, after 7 days' notice,
section 8.2). A creator can set tighter ones, never looser. A new mandate or new limits wait
a day, so holders who disagree can redeem first, and the creator or the guardian can pause the
reviews. A Smart basket starts with at least $50.

Its protocol fee is 0.80% per mint and redeem (ceiling 1.00% in the code), split like the
normal fee, for the daily reviews. Making a basket Smart burns 50,000 GRAVI once (ceiling
1,000,000), on top of the per-asset burn; assets a review adds later burn nothing.

## 4. Rebalancing

Prices move, so a basket's actual weights drift from its targets. When a basket's schedule
comes round, or it drifts past its tolerance, a keeper trades it back:

- **In slices** of at most 5% of the basket's value, so no single trade moves a thin market. An
  asset on its way out (set to 0% by its creator, or stopped out, section 4.1) sells in clips of
  up to 50%, each still stopping at the bought asset's target.
- **At unpredictable times**, so nobody can trade ahead of a known schedule.
- **Against a price floor.** Each slice must fetch at least the oracle price, less a small
  tolerance (1% for stocks and crypto; up to 3%, the program's ceiling today, when a memecoin is
  involved, whose pool fees alone can pass 1.5%), or the whole slice reverts. The keeper is lent the tokens to sell, and the program
  checks what comes back to the vault in the same transaction.

The program checks every slice, and the share supply can't change in the middle of one. The
floor is only as good as the price it's checked against. A price service posts those prices
with its own key, the program's price authority: it reads each price from Jupiter itself, so the
app can ask it to post a price but never choose one. The program bounds what it may post. Where an asset has a Pyth
feed (about 20 of them today: SOL, ETH, BTC, JUP, BONK, WIF and others), a posted price must be
within 3-5% of a fresh, fully verified Pyth price that isn't dated in the future. Any other
asset's price may move at most 20% from the last accepted one per hour, so a false price can't
appear at once; its very first price only sets that reference, and nothing trades on it until a
price posted at least 15 minutes later agrees with it within those 20%. Within those bounds
a rebalance can't take value out of a basket as long as the prices posted are honest. Section 8
says who holds that key.

**Only baskets holding at least $50.** The keeper doesn't rebalance a basket worth less than
$50: nothing is traded, not for drift, not on schedule, not even an asset its creator is adding
or removing. Its holdings drift with the market until it grows past $50; minting and redeeming
work as usual. Below that a slice is a few cents, and hundreds of small baskets would take
turns from the real ones. Each basket's page says when it's below the line.

**The keeper is paid back, not paid.** Running a basket costs the keeper network fees: each
slice, the prices posted for it, and once, the basket's rebalance record. Each slice pays it back
what it spent on that basket so far, in the token the slice sells, and at most $0.05 a slice,
a limit the program enforces. What isn't covered yet waits for the next slice. The keeper sells
those tokens for SOL. Holders pay that cost as a sliver of the sold token, and nobody earns
anything from it. Sweeps pay back the same way. A buyback run pays the keeper back for all its
sales at once, at most $0.50, only out of the burn's budget, and selling the stakers' tokens for
SOL works the same way out of theirs.

**Taking turns.** The keeper runs a few slices an hour, shared by every basket. The baskets that
want a slice go in order of how long they've waited since their last one: a basket goes to the
back of the queue each time it's served, or when its slice fails. So a volatile basket that's
due every hour can't take the turns of the others, and every basket gets one in its turn.

**Tokens a basket is sent.** Some tokens pay their holders in another token (a stonkfun token
can pay ZEC), and anyone can send a basket an airdrop. Those tokens aren't part of the recipe,
so the keeper sweeps them: it swaps them into the basket's most underweight token against the
same oracle floor, and their value goes to the holders.

**A token the market stops pricing.** An asset at 0% whose price hasn't been posted for a day
counts as worth nothing in the books, so it can't block the rest of the basket. Nothing can be
sold or bought at $0, so it stays in the vault and redeeming still pays it out.

### 4.1 Stop losses

A creator can set a stop loss on any of a basket's assets: a percentage, 5% to 90%, below a
reference price. A fixed stop's reference is the asset's price when the stop was set ("down 30%
from now"); a trailing stop's reference rises with higher prices that have held (the asset's
guard reference, section 4, at least 5 minutes old), so it sits below the asset's highest
lasting price since; one high print doesn't lift it. On a basket with holders a reference can't be
above the asset's posted price when the stop is written, so no stop starts out triggered. The
stops are stored on chain, where anyone can read them.

If the asset's posted price is below its stop, the stop arms; if it's still below 5 to 15 minutes
later, it fires. An arming nobody confirmed within 15 minutes starts over. The program checks both
prices (each fresh, within the bounds of section 4) and the time between them, and anyone may ask
it to: the keeper does every minute. A price back above the stop before then disarms it. Firing
sets the asset's target to 0% and scales every other target up in proportion, so the basket is at
100% again without it, and the stop is gone. On a smart basket, or one its holders run (section
8.1), no asset is scaled past its weight cap: what the caps leave no room for stays on the stopped
asset, and a stop with no room at all waits, armed. The keeper then
sells the asset out in clips of up to half the basket, a minute apart, each into the asset
furthest under its new target and each held to the oracle floor. This happens whatever the
basket's size: the $50 line doesn't apply to a stop.

What a stop can't do: it doesn't buy back if the price recovers; a creator can add the asset
again. A stop fires on the posted price, so on an asset without a Pyth feed, whose price may fall
at most 20% an hour, a stop after a crash fires and sells in steps, over hours; the app labels
these "slow stops". A thin token can be pushed through its stop for 5 minutes, and a stop sells
near the bottom of a dip. An immutable basket's stops are part of its recipe: they're set before
its first deposit and never change, and when one fires the basket follows that fixed rule.

## 5. Fees

| Fee | Rate | Who pays, when | Where it goes |
|---|---|---|---|
| **Protocol fee** | 0.40% | On every mint and redeem, including a creator's first deposit | Half to the GRAVI burn, a tenth to GRAVI stakers (section 6.4), the rest to the treasury |
| **Smart basket fee** | 0.80% | Replaces the protocol fee on a Smart basket's mints and redeems | Split like the protocol fee |
| **Creator's cut** | 0% to 0.60% | On each buy by someone else, on top of the protocol fee. Selling and holding don't pay it | The basket's creator |
| **Yearly fee** | 0% to 0.20% a year | Holders, as share dilution while held, on baskets that haven't chosen a cut | The basket's creator |
| **Creation burn** | 10,000 GRAVI per asset | The creator, for each asset a basket is created with or gets added, unless a GRAVI stake covers it (section 6.4) | Burned |
| **Smart burn** | 50,000 GRAVI | The creator, once, on making a basket Smart | Burned |
| **App fee** | 0.05% | On basket buys and sells through the app's USDC/SOL route, and on single-token trades | The treasury |
| **Network** | fractions of a cent | Every transaction | Solana validators |

**Invited wallets.** A wallet that joins with someone's invite doesn't pay the GRAVI-burn half of
the protocol fee for its first 30 days: it pays 0.20% (0.40% on a Smart basket), none of it
burned: a tenth of the full fee goes to stakers as usual, the rest to the treasury. The app fee is
unchanged. The program reads the wallet's invite record, good for at most 31 days, which the
wallet writes with its first trade and the keeper co-signs. The wallet puts down the record's
rent, under 0.001 SOL, and gets it back with its first trade after the 30 days. For a year, the inviter receives a quarter of the protocol fee on
that wallet's basket trades (0.10% of each trade, 0.20% on a Smart basket), worked out in SOL at each trade's price and
paid daily in SOL by the app. It is a share of fees from someone else's trading, not a return on
anything held, and it never pays out more than those trades paid in.

The protocol fee rounds up, so no flow is too small to pay it, and it is kept in shares, so
the vault stays fully backed. A creator's cut rounds up too and waits in the treasury's shares
until it's claimed, by anyone, to the creator's own account; the treasury can't withdraw it. The
limits are in the program's code: the protocol fee can never
exceed 0.50% (1.00% on a Smart basket), a creator's cut can never exceed 0.60% of a buy, a yearly fee can never exceed 0.20%, the creation burn can never exceed
1,000,000 GRAVI per asset (the Smart burn 1,000,000 GRAVI once), and at least half of every protocol fee goes to the GRAVI burn, except
an invited wallet's half fee in its first month (above). Changing
any of them would take new code, in public: the values the protocol authority can tune without
an upgrade (section 8.2) include none of these ceilings.

Opening a token account the first time a wallet holds an asset costs about 0.002 SOL of
Solana rent, refunded when the account is closed. Trading shares wallet to wallet, or in any
pool someone else runs, pays the protocol nothing.

## 6. GRAVI

GRAVI is GraviFi's token. It carries no vote and no claim on baskets. Its main job is to absorb
the protocol's revenue through burns: the more GraviFi is used, the less GRAVI there is. Staking
it (section 6.4) shares part of the protocol fee and removes the creation burn.

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

GRAVI's pump.fun creator fees are claimed into the team's GRAVI wallet, about once a week: half
buys GRAVI and burns it, half goes to the treasury.

The first burn (15 October 2026) bought and burned 10,611,685 GRAVI with 1.90 SOL. By 7 November
2026 all burns together had taken about 126 million GRAVI out of supply, 12.6% of it, most of it
from these trading fees. Every claim and burn is public on Solana, and the live total is at
[app.gravifi.xyz/pulse](https://app.gravifi.xyz/pulse).

### 6.3 Creation burn

Every asset in a basket burns GRAVI from the creator's wallet: creating a basket burns it for
each asset the basket starts with, in the instruction that creates it (a basket created and
never seeded has still paid it), and adding an asset later burns it for that asset. It is a
real burn, so supply falls; nobody receives it. The amount is **10,000 GRAVI per asset**
today (100,000 until 25 October 2026), so a five-asset basket burns 50,000. The protocol authority sets it (section 8), and 0
turns it off; the program refuses anything above **1,000,000 GRAVI per asset**, and only a
program upgrade could raise that ceiling. A creator doesn't need to hold GRAVI first: the app
buys whatever is missing with SOL as part of creating or adding, and shows the cost before
signing.

One wallet can be exempt: the protocol authority can name a single team wallet whose baskets
burn nothing, so the team can launch its own promotional baskets without spending GRAVI it
doesn't have. Which wallet it is, if any, is public on-chain (right after the protocol
account's fields), and the exemption applies to that wallet's baskets only. A GRAVI stake also
makes free baskets: one a day per 5,000,000 GRAVI staked (section 6.4).

### 6.4 Staking

Anyone can stake any amount of GRAVI. Two things follow:

- **A share of the fee.** 10% of every protocol fee goes to stakers, out of the treasury's side:
  50% burned, 10% to stakers, 40% to the treasury. The program counts it on each basket as fees
  are paid, the treasury can't withdraw it, and anyone can move it to the rewards account, where it
  is redeemed and sold for SOL. Every 30 days the SOL collected becomes a pot, paid out second by
  second in proportion to each stake that counts, to claim at any time.
- **Free baskets.** Each **5,000,000 GRAVI** staked makes one basket a day with no GRAVI burned:
  5M gives one a day, 10M two, and so on. A free basket covers creating it, its assets, and making it
  smart while it's created; adding an asset to a basket later uses one too. Unused ones don't carry
  over. The protocol authority can move the 5,000,000, up to a ceiling of 10,000,000 in the code; a
  raise applies only after 7 days' notice.

Staked GRAVI counts 2 days after it's staked; a top-up waits its own 2 days, and what already counts
keeps counting. Unstaking takes 7 days, during which that GRAVI earns nothing and makes no free
basket. Staked GRAVI sits in the stake pool's own account, which only its staker's
withdrawal moves: the account that sells for the pot can't reach it.

What the pot pays depends only on what baskets earn in fees. It can be very little, and it is not
a promise of any return. GRAVI's price can fall by more than a stake earns.

## 7. Assets

142 assets are GraviFi Verified today:

- **Tokenized stocks and funds** issued on Solana (xStocks and others): NVIDIA, Apple, Tesla,
  the S&P 500, the Nasdaq 100, gold and more.
- **Pre-IPO tokens**: Anthropic, OpenAI, Anduril, Polymarket and others.
- **Crypto**: SOL, bitcoin (wBTC, cbBTC), ETH, HYPE, ZEC, Jupiter, TAO and more.
- **Memecoins**: from BONK and WIF to stonkfun's tokens.

An asset is listed only if it has a working market on Solana. A token whose own rules let its
issuer take a cut of every transfer is accepted only up to 4%, and the program checks that
cap on every deposit. Tokens with an active transfer hook, which hands every transfer to
another program, are refused.

Tokenized stocks are issued by third parties, who can freeze or claw back their tokens under
their own terms; GraviFi can't prevent that, only keep redemption of everything else open
(section 2.2). Their issuers don't offer them to US persons.

### 7.1 Verified and Unverified

The assets above are **GraviFi Verified**: chosen by the team. A creator
can also ask for any other Solana token with a market (open listing). The app checks it
first and refuses, whatever else it has going for it, a token that could jam or empty a
vault: an active transfer hook, a transfer fee over 4%, accounts that start frozen, a token
that can't move or is paused, and freeze, clawback or pause powers held by anyone but a known
stock, gold or stablecoin issuer. Leveraged and inverse products are allowed, with a warning
that they lose value when the market swings. Beyond that it needs a live price, $10,000 in
its pools, 6 hours of trading, and a $150 trade either way within 10% of the market. A token that passes is offered to everyone as **Unverified**
and registered on chain in the transaction of the first basket that uses it, signed by the
app's listing key (the lister, which the program lets list assets and nothing else); that
creator pays the record's rent.

Unverified tokens carry a badge and a one-line note wherever they appear, and so does every
basket holding one; Explore and Pulse can be narrowed to baskets of Verified tokens. The
rebalancer gives them the wider limits memecoins get, and backs off a basket whose slices
keep failing on one. GraviFi doesn't fix or refund problems an Unverified token causes. The
app re-checks them in turn every day: one that fails the rules for 3 days running is no
longer offered for new baskets (baskets holding it keep it), and one Jupiter hasn't priced
for a day is valued at zero so its baskets stay tradable. A token that meets the Verified bar for a week (for stocks from a
known issuer, a $150 trade within 2-3%; for others, also $1M market cap and $100,000 in
pools) can be promoted by the team. Every token shows its contract address.

## 8. Who controls what

A basket is run by its creator, by the strategist within its limits if it's Smart (section 3.1),
or by its holders (section 8.1); there is no vote on the protocol. Control is split so that no role can reach holders' funds:

| Role | Can | Can't |
|---|---|---|
| **Holders** | Redeem at any time, send or trade their shares | |
| **A basket's creator** | Change their basket's weights, assets, rebalancing, cut (within the caps; a rise waits a day, and lowering the cut calls off a rise still waiting), stop losses, split; on a Smart basket, set its mandate and tighter limits (each waits a day) and pause it; set how wallets show its shares (name, ticker, picture); set an immutable basket's stop losses before its first deposit | Touch the vault's tokens; once the basket is immutable, anything but how wallets show its shares |
| **The lister** (the app's listing key) | List a token a creator picked, inside the creator's own transaction, once it passed the app's checks | Anything else: delist, change a setting, move a token; or list a token whose new accounts start frozen, that can't move, or that's paused |
| **The strategist** (the app's Smart-basket key) | Set a Smart basket's targets, add and remove its assets and put its mandate's stops on them, within that basket's limits | Move a token, change a fee or a limit, touch any basket that isn't Smart |
| **The protocol authority** (the team) | List and delist assets, name the lister, the strategist and the guardian (a key named acts after 24 hours), set fees, the Smart fee, the creation burn and the Smart burn within the ceilings (a raise waits 7 days), name the one burn-exempt team wallet, move the staking threshold (a raise waits 7 days), open or close community mode, tune the values and use the kill switches in section 8.2 within their bounds (user-facing changes wait 7 days), withdraw the treasury's shares (never what's owed to creators or stakers), match assets to Pyth feeds, name the keepers, set how wallets show any basket's shares, and upgrade the program | Through any of its settings: move a vault's tokens, block a redemption, exceed the fee or creation-burn ceilings or any bound in section 8.2, send less than half of the protocol fee to the burn (outside an invited wallet's half fee) |
| **The price service** (its own key) | Post the prices rebalances, buybacks and stop losses are checked against, within the bounds above, read from Jupiter by itself | Anything else: trade, move a token, change a setting |
| **The guardian** (none named today) | Delist an asset; pause a Smart basket's reviews | List anything, or anything else |
| **The guard** (a separate server key) | Co-sign, with the app's key, a recurring plan's due fill or a hit stop's sale from that plan's or stop's own wallet (section 2.4) | Sign alone; touch a holder's main wallet; send funds anywhere but back to the owner |
| **The keeper** (the app's server) | Run rebalance, sweep and buyback slices within the program's checks, paying it back at most $0.05 a rebalance or sweep slice and $0.50 a buyback or stakers' sale, for its network costs; ask the price service to post a price (anyone may ask the program to check a stop); co-sign an invited wallet's invite record | Post or choose a price; move a vault's tokens outside a slice, or accept less back than the posted price allows; take more than those limits; give a wallet half fees for more than 31 days at a time |
| **The burn address** | Buy GRAVI and burn it | Anything else: it has no key |

The program and protocol authority are held today by one team key, and upgrading a program
can change what it does. Prices and trades take two more keys, held apart: the price service's
key posts prices, the app server's key trades. Taking value through a false price needs both. The
price key alone can post a price only within the bounds above, which could send a rebalance the
wrong way or fire a stop early, but it can't trade; the trading key alone trades only at the
prices the price service posted. Holders trust the team with these keys today. That is why the next steps (section 10) are a multisig, so no single
key can upgrade it, and a verified build, so anyone can check that what runs on-chain is the
published code.

### 8.1 Community baskets

A creator can hand a seeded, public basket to its holders. It is one way: only a vote can return
it. The creator keeps any cut or yearly fee, which a vote can only lower, and loses every other
power over the basket.

Anyone holding a basket's shares can stake them, on any basket; they keep their value but can't be
sold or redeemed until withdrawn. In a community basket, staked shares vote:

- **Weight.** One share, one vote, built up over 3 days and read as of the moment a proposal
  opened, so shares bought or staked after that carry nothing on it.
- **Proposing.** Staked weight of 1% of the supply, at most 3 open proposals per basket, and one
  per wallet.
  Proposals can change targets, add an asset, change the rebalancing policy, the stops or the
  split, lower the creator's fee or cut, hand the basket to the strategist, or return it to its
  creator.
- **Passing.** At least 10% of the supply votes, and over half of those voting agree (66% to change
  the creator's fee or cut, go smart, or leave). The vote takes 3 days and a passed change waits 24
  hours, so anyone who disagrees can redeem first. Anyone can then run it, for 7 days.
- **Limits.** Every change stays within the ceilings a smart basket's strategist is held to: 35%
  in one asset (30% if not Verified), 40% of the targets changed in a day, 100% in a week. Stop
  losses fire with no vote, within the same weight caps. An asset sold out can be removed by
  anyone, but not while a proposal is open, nor for 11 days after a vote added one, so a new asset
  can't be dropped before the vote that weights it runs. Redeeming is always open.

A majority can still do what its votes allow: within those limits it could replace a whole basket
in about 6 days. A vote can be bought, and nothing on chain can stop that. Holders of a community
basket are trusting its other holders, and can leave at any time.

### 8.2 What the protocol authority can tune, and when

Most of the numbers in this paper live in one account the protocol authority sets, so tuning them
doesn't need a program upgrade. Each has bounds written in the code that no setting can pass, and
none of them is a fee ceiling or the burn's floor: those stay in the code (section 5). Until a
value is set it is the number this paper states.

- **At once:** the longest invite (1 to 93 days).
- **Tighter at once, looser after 7 days:** every value that bounds the app's own keys, so a lost
  or misused team key can't loosen them overnight. Price checks (a move of 2%-30% per reference
  without Pyth, over a reference of 15 minutes to a day; Pyth at most 15 minutes old and within
  0.5%-10%), how fresh a price must be to trade on (30 seconds to 10 minutes), the keeper's
  paybacks (at most $0.25 a slice and $2 a buyback run), the buyback's tolerance (0.5%-15%; it
  sells fees, never a deposit), a rebalance's tolerance (0.1% to 5%), the slice cap (0.5% to 10%
  of a basket) and a leaving asset's clip (5% to 100%).
- **After 7 days' notice:** when an unpriced 0% asset counts as nothing (1 hour to 30 days), and
  everything holders and creators live by. Stop losses (confirmation 1
  to 60 minutes, the arming window 2 minutes to 3 hours, a stop 1%-20% to 50%-95% below its
  reference), the policy minimums (a drift of 0.1%-5%, a schedule hourly to weekly), the
  transfer-fee cap on new money (1% to 8%), the split cooldown (1 to 90 days), a cut rise's notice
  (6 hours to 30 days), a mandate's notice (6 hours to 14 days), the Smart ceilings (5%-60% a day,
  10%-150% a week, 10%-60% in one asset, 1%-50% unverified), staking's warm-up (1 hour to 14
  days), unstaking (1 to 30 days) and a pot's month (7 to 90 days), and votes: voting 1 to 14
  days, the wait 1 hour to 7 days, the window to run it 1 to 30 days, proposing 0.1%-10% of the
  supply, quorum 1%-50%, a supermajority of 51%-90%, 1 to 10 open proposals.

A change never moves a window already open: a proposal's vote, wait, window and passing rules,
a stake's warm-up, an unstaking, a cut rise and a mandate keep the times and rules they started
with.

**Fees, burns and keys.** The fees and burns keep their ceilings in the code (section 5), and
changing them follows the same rule as the values above. A raise of the protocol fee, the Smart
fee, the burn's share of the fee, the creation burn or the Smart burn waits 7 days, then applies
by itself; lowering one is at once, and calls off a raise still waiting. So does a raise of the
staking threshold. A key the authority names for a role (a new authority, the guardian, the
lister, the strategist, the price service's key, the keeper's key, the burn-exempt wallet) acts
only 24 hours later, the key it replaces acting until then, so a key swapped in is public before
it can do anything. Removing a role is at once: it only stops that key, and is how a lost key is
cut off.

**Kill switches.** The authority can stop, at once, a family of instructions: new money in, new
baskets, rebalancing, the buyback, stop losses, Smart baskets' strategist, new stakes, community
votes, invites, or the lister. A switch only stops things; lifting it returns to the rules above.
No switch reaches the way out: redeeming, unstaking and withdrawing, claiming, closing stops,
pausing a Smart basket and handing a basket back to its creator always work. A new feature can be
shipped switched off and turned on later, after the same 7 days' notice.

## 9. Risks

GraviFi is new software. Before using it, know that:

- **No independent audit yet.** The program has passed an internal security review and its
  test suite; an independent audit is next. Only put in what you can afford to lose.
- **Key risk.** The team holds the keys that upgrade the program and post the prices
  rebalances are checked against (section 8). Posted prices are bounded (section 4), and the
  price and trading keys are held apart, but until the multisig lands a lost or misused team key
  could cost basket holders money.
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
3. **Pyth checks for more assets** (the tokenized stocks' feeds) as Pyth keeps them updated on
   chain.
4. **Baskets of up to 16 assets.**

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
