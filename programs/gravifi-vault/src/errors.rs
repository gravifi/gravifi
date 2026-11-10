use anchor_lang::prelude::*;

#[error_code]
pub enum VaultError {
    #[msg("Arithmetic overflow")]
    MathOverflow,
    #[msg("Target weights must sum to exactly 10000 bps")]
    WeightsNotNormalised,
    #[msg("Too many constituents")]
    TooManyConstituents,
    #[msg("A basket needs at least two constituents")]
    TooFewConstituents,
    #[msg("Duplicate constituent mint")]
    DuplicateConstituent,
    #[msg("Basket has already been seeded")]
    AlreadySeeded,
    #[msg("Basket has not been seeded yet")]
    NotSeeded,
    #[msg("Shares requested must be greater than zero")]
    ZeroShares,
    #[msg("This basket is immutable and can never be reconfigured")]
    BasketImmutable,
    #[msg("Signer is not authorised to change this basket's mandate")]
    NotMandateAuthority,
    #[msg("Minting into this basket is restricted")]
    MintNotPermitted,
    #[msg("Wrong number of remaining accounts: expect one [vault_ata, user_ata, mint, token_program] quad per constituent")]
    BadRemainingAccounts,
    #[msg("Remaining account does not match the expected constituent mint")]
    ConstituentMismatch,
    #[msg("Vault holds less than the units promise; call sync_units first")]
    VaultUndercollateralised,
    #[msg("Constituent mint has an enabled transfer hook, which this version cannot route")]
    TransferHookUnsupported,
    #[msg("Fee exceeds the protocol ceiling")]
    FeeTooHigh,
    #[msg("Share supply is zero")]
    ZeroSupply,
    #[msg("Signer is not the protocol authority")]
    NotProtocolAuthority,
    #[msg("Constituent is not a listed asset")]
    AssetNotListed,
    #[msg("Minting is paused: this basket holds a delisted asset")]
    MintPaused,
    #[msg("Parameter out of range")]
    BadParameter,
    #[msg("Signer is not the configured price authority")]
    NotPriceAuthority,
    #[msg("Signer is not the configured rebalancer")]
    NotRebalancer,
    #[msg("Price is missing, stale or for another asset")]
    BadPrice,
    #[msg("This basket isn't due for a rebalance")]
    RebalanceNotDue,
    #[msg("The sleeve to sell must be overweight and the sleeve to buy underweight")]
    WrongDirection,
    #[msg("Slice is larger than the drift or the per-slice cap allows")]
    SliceTooLarge,
    #[msg("A rebalance slice is already in progress")]
    RebalancePending,
    #[msg("rebalance_start must be followed by rebalance_finish in the same transaction")]
    FinishMissing,
    #[msg("No rebalance slice is in progress")]
    NoRebalancePending,
    #[msg("The swap returned less than the oracle floor allows")]
    BelowOracleFloor,
    #[msg("Share splits need a Token-2022 share mint with a scaled UI amount")]
    SplitUnsupported,
    #[msg("A basket can split at most once every 30 days")]
    SplitTooSoon,
    #[msg("Holder governance has been removed: baskets are creator-managed or immutable")]
    GovernanceRemoved,
    #[msg("Only a basket that was handed to its holders can be returned to its creator")]
    NotDaoBasket,
    #[msg("Vault account must be the basket's associated token account for that mint")]
    NotBasketVault,
    #[msg("Share supply changed during a rebalance or buyback slice")]
    SupplyChanged,
    #[msg("The vault received less than the deposit: the token charges on transfer")]
    DepositShort,
    #[msg("The GRAVI mint is already set")]
    GraviMintAlreadySet,
    #[msg("The GRAVI mint isn't set yet")]
    GraviMintNotSet,
    #[msg("A buyback slice is already in progress")]
    BuybackPending,
    #[msg("buyback_start must be followed by buyback_finish in the same transaction")]
    BuybackFinishMissing,
    #[msg("No buyback slice is in progress")]
    NoBuybackPending,
    #[msg("The wallet's tokens cover fewer shares than the minimum asked for")]
    BelowMinShares,
    #[msg("Set this asset to 0% and let rebalancing sell it out before removing it")]
    ConstituentNotEmpty,
    #[msg("Rebalancing must wait at least a day between scheduled runs and tolerate at least 0.5% drift")]
    BadPolicy,
    #[msg("This token's transfer fee is above the 3% GraviFi accepts")]
    TransferFeeTooHigh,
    #[msg("Too few shares: the protocol fee would take all of them")]
    BelowFee,
    #[msg("The burn's share of the protocol fee can't be under 50%")]
    BurnShareTooLow,
    #[msg("Slice too small: the least the vault would accept back rounds to zero")]
    SliceTooSmall,
    #[msg("This token is one of the basket's own: rebalance it, don't sweep it")]
    NotStray,
    #[msg("The GRAVI burned per asset can't be over 1,000,000 GRAVI")]
    CreationBurnTooHigh,
    #[msg("Each asset in a basket burns GRAVI: pass the GRAVI mint, your GRAVI account and its token program")]
    CreationBurnAccountsMissing,
    #[msg("Only the basket's creator or the protocol authority can name its share token")]
    NotMetadataAuthority,
    #[msg("The share's name must be 1 to 32 bytes, its symbol 1 to 10 and its URI at most 200")]
    BadMetadata,
    #[msg("That price moved too far from the last accepted one: at most 20% an hour")]
    PriceMoveTooLarge,
    #[msg("This asset has a Pyth feed: pass its price update account")]
    PythPriceMissing,
    #[msg("That isn't a fully verified Pyth price update for this asset's feed")]
    PythPriceInvalid,
    #[msg("Pyth's price for this asset is too old to post against")]
    PythPriceStale,
    #[msg("That price is too far from Pyth's")]
    PriceOffPyth,
    #[msg("This basket has no shares out: only its creator can open it again")]
    BasketEmpty,
    #[msg("That invite is wrong: not this wallet's, or running past a month")]
    BadInvite,
    #[msg("This invite hasn't run out yet")]
    InviteActive,
    #[msg("A slice pays the keeper back at most $0.05")]
    RefundTooLarge,
    #[msg("New accounts of this token start frozen: a vault couldn't hold it")]
    DefaultFrozen,
    #[msg("This token can't be transferred")]
    NonTransferable,
    #[msg("This token's transfers are paused")]
    TokenPaused,
    #[msg("An immutable basket's stops are set before its first deposit and never change")]
    StopsFixed,
    #[msg("A stop must be on one of the basket's assets, once each, 5% to 90% below a positive reference price")]
    BadStop,
    #[msg("That asset has no stop in this basket")]
    StopNotSet,
    #[msg("That asset isn't in the basket at a target above 0%")]
    StopInactive,
    #[msg("The basket's last asset with a target can't be stopped out: there's nothing to move into")]
    StopLastAsset,
    #[msg("A burn token account is closed only once it's empty")]
    BurnAccountNotEmpty,
    #[msg("The burn PDA's GRAVI and share accounts are never closed")]
    BurnAccountKept,
    #[msg("Nothing of this program's may run between a buyback's start and finish, nor a burn redeem from inside another program")]
    BuybackInterleaved,
    #[msg("Only the strategist can do this, on a smart basket")]
    NotStrategist,
    #[msg("This isn't a smart basket")]
    NotSmartBasket,
    #[msg("This smart basket is paused: it stays on its last targets")]
    SmartPaused,
    #[msg("A smart basket's targets and assets are set by its strategist, within its guardrails")]
    SmartManaged,
    #[msg("A basket becomes smart before its first deposit, from creator control")]
    SmartNotAllowed,
    #[msg("Guardrails out of range: at most 20% a day, 50% a week, 35% an asset (30% unverified), 2 to 14 assets, a stop 5% to 90%, and a mandate hash")]
    BadGuardrails,
    #[msg("That weight is over the basket's cap for this asset")]
    WeightAboveCap,
    #[msg("That many assets is outside the basket's mandate")]
    AssetCountOutOfRange,
    #[msg("That changes the targets more than the basket allows in 24 hours")]
    TurnoverDayCap,
    #[msg("That changes the targets more than the basket allows in 7 days")]
    TurnoverWeekCap,
    #[msg("No new mandate is waiting")]
    NoPendingMandate,
    #[msg("A new mandate applies only after a day's notice")]
    MandateNoticeRunning,
    #[msg("Only the creator or the guardian can pause a smart basket, and only who paused it (or the guardian) can resume it")]
    NotPauser,
    #[msg("Wrong Verified record for this asset")]
    BadVerifiedRecord,
    #[msg("This basket's mandate has no stop, or that asset already has one")]
    SmartStopNotAllowed,
    #[msg("A slice through SOL must buy the basket's own wrapped SOL")]
    NotSolHub,
    #[msg("A basket with shares out raises its creator cut with a day's notice")]
    CutRiseNeedsNotice,
    #[msg("This rise's notice hasn't run out yet")]
    CutNoticeRunning,
    #[msg("These treasury shares are owed to the basket's creator")]
    TreasuryOwedToCreator,
    #[msg("Staking isn't set up on this cluster")]
    StakingOff,
    #[msg("Over the staking threshold's ceiling, or zero")]
    BadStakeThreshold,
    #[msg("That's more than is staked")]
    NotEnoughStaked,
    #[msg("This GRAVI is still waiting out its unstaking days")]
    UnstakeWaiting,
    #[msg("Nothing to withdraw or claim")]
    NothingToTake,
    #[msg("Close a stake only once it's empty and paid")]
    StakeNotEmpty,
    #[msg("This month's pot is still paying out")]
    StreamRunning,
    #[msg("Wrong stake account")]
    BadStake,
    #[msg("The treasury can't go below what stakers are owed")]
    TreasuryOwedToStakers,
    #[msg("Community baskets aren't open")]
    CommunityClosed,
    #[msg("Only a seeded, public, creator-managed basket can go community")]
    CommunityNotAllowed,
    #[msg("Holders run this basket by vote")]
    CommunityManaged,
    #[msg("This basket isn't run by vote")]
    NotCommunity,
    #[msg("Your staked shares carry too little weight to propose: 1% of the supply")]
    BelowProposalWeight,
    #[msg("This basket has as many open proposals as it can")]
    TooManyProposals,
    #[msg("That change isn't possible on this basket as it stands")]
    BadProposal,
    #[msg("Voting on this proposal has ended")]
    VotingOver,
    #[msg("Your staked shares had no weight when this proposal opened")]
    NoVotingWeight,
    #[msg("Staked shares stay a day after staking, and shares that voted until the vote ends")]
    SharesLocked,
    #[msg("This proposal can't execute: it hasn't passed, is waiting, has lapsed or already ran")]
    NotExecutable,
    #[msg("This proposal can't close yet")]
    ProposalStillOpen,
    #[msg("The basket's assets changed since this proposal opened")]
    AssetsChanged,
    #[msg("A stop's reference can't be above the asset's fresh posted price")]
    StopAbovePrice,
    #[msg("The guardrails leave no room for this stop's weight")]
    StopNoRoom,
    #[msg("Nothing else of GraviFi's may run between a slice's start and its finish")]
    SliceInterleaved,
    #[msg("An asset a vote added can't be removed until its reweight vote has had its time")]
    AssetJustAdded,
    #[msg("An asset can't be removed while a proposal is open")]
    RemovalWaitsForVotes,
    #[msg("You already have an open proposal on this basket")]
    ProposerHasOpenProposal,
    #[msg("Accrue the creator's fee in the same transaction first")]
    FeeNotAccrued,
    #[msg("The protocol has paused this for now")]
    FeatureOff,
    #[msg("Wrong params account")]
    BadParams,
    #[msg("No such setting")]
    UnknownParam,
    #[msg("That setting is outside the bounds written in the program")]
    ParamOutOfBounds,
    #[msg("No such role, or a wrong account for it")]
    BadRole,
    #[msg("Wrong basket extension account")]
    BadExt,
    #[msg("The basket's extension account is full")]
    ExtFull,
    #[msg("This rise was called off: the cut was lowered after it was asked for, or it's no higher than the cut now")]
    CutRiseVoid,
    #[msg("The Pyth price is published in the future")]
    PythPriceAhead,
    #[msg("Hand the basket to another wallet: not the empty address, nor the one running it now")]
    BadNewManager,
    #[msg("Not one of our parts: its mint, OFT store, store admin or peer doesn't check out")]
    NotAPart,
    #[msg("This asset is already Verified")]
    AlreadyVerified,
    #[msg("Only the trip key starts a cross-chain trip")]
    NotTripKeeper,
    #[msg("Value is moving between chains for this basket: try again in a few minutes")]
    TripOpen,
    #[msg("No trip is out for this basket, or it isn't this one")]
    TripNotOpen,
    #[msg("The trip isn't past its deadline yet")]
    TripNotDue,
    #[msg("A trip missed its deadline: no new trip until the guardian clears it")]
    TripStuck,
    #[msg("The trip hasn't settled yet")]
    TripNotSettled,
    #[msg("A trip is between a part and another sleeve")]
    TripNeedsPart,
    #[msg("A trip's tokens go only to the bridge-out address the authority named")]
    NotBridgeOut,
    #[msg("Shares are still out, or something is still owed to the creator or the stakers")]
    BasketNotEmpty,
    #[msg("Close the basket's vaults first")]
    VaultsOpen,
    #[msg("A community proposal is still open: close it first")]
    BasketBusy,
    #[msg("A failed trip settles only by the trip key, the bridge-out address, the guardian or the authority")]
    LateSettleNotAllowed,
    #[msg("The trip's token account still holds tokens: settle it instead")]
    TripVaultNotEmpty,
    #[msg("This IOU's trip is still open: claim it once it settles")]
    TripStillOpen,
    #[msg("Leave at least two of the basket's assets without a stop")]
    TooManyStops,
    #[msg("That isn't this basket's USDC pot for its share stakers")]
    BadPot,
    #[msg("A trip's id is the unix time it starts")]
    BadTripId,
    #[msg("What the trip was to bring back has arrived: settle it instead")]
    TripDelivered,
    #[msg("No weights have been announced for this basket")]
    NoAnnouncedTargets,
    #[msg("The announced weights take effect when their notice is over")]
    TargetsNotDue,
}
