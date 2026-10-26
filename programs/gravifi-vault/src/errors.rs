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
}
