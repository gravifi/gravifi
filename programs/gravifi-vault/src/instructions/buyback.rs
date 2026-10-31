//! GRAVI's buyback and burn: burn-earmarked shares are redeemed into the burn PDA, its tokens
//! sold for SOL against the oracle, the SOL sold for GRAVI in one go, and every GRAVI it then
//! holds is burned.

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{self, Mint as MintI, TokenAccount as TokenAccountI, TokenInterface};

use crate::errors::VaultError;
use crate::state::*;
use crate::token_util::*;
use anchor_lang::solana_program::sysvar::instructions::{load_current_index_checked, load_instruction_at_checked};

use crate::valuation::token_value;

/// Turns burn-earmarked shares of one basket into its underlying tokens, held by the
/// burn PDA, ready to be sold for GRAVI. No fee, no price: exactly what a redeem pays.
/// Permissionless, since value only moves from the basket to the burn PDA.
///
/// Remaining accounts: one `[vault_ata, burn_ata, mint, token_program]` quad per
/// constituent, `burn_ata` being a token account the burn PDA owns.
pub fn buyback_redeem<'info>(
    ctx: Context<'_, '_, 'info, 'info, BuybackRedeem<'info>>,
    shares: u64,
) -> Result<()> {
    require!(ctx.accounts.basket.seeded, VaultError::NotSeeded);
    require!(shares > 0, VaultError::ZeroShares);
    // Never from inside another program: a sale's finish would count what this delivers as
    // what the sale brought back (`finish_follows` sees only a transaction's own instructions).
    require!(
        anchor_lang::solana_program::instruction::get_stack_height() == anchor_lang::solana_program::instruction::TRANSACTION_LEVEL_STACK_HEIGHT,
        VaultError::BuybackInterleaved
    );
    let count = ctx.accounts.basket.constituent_count as usize;
    require!(ctx.remaining_accounts.len() == count * 4, VaultError::BadRemainingAccounts);
    let supply_before = ctx.accounts.share_mint.supply;

    let burn_bump = [ctx.bumps.burn_authority];
    let burn_seeds: [&[u8]; 2] = [BURN_SEED, &burn_bump];
    token_interface::burn(
        CpiContext::new_with_signer(
            ctx.accounts.share_token_program.to_account_info(),
            token_interface::Burn {
                mint: ctx.accounts.share_mint.to_account_info(),
                from: ctx.accounts.burn_shares.to_account_info(),
                authority: ctx.accounts.burn_authority.to_account_info(),
            },
            &[&burn_seeds],
        ),
        shares,
    )?;

    let basket_key = ctx.accounts.basket.key();
    let burn_key = ctx.accounts.burn_authority.key();
    let creator = ctx.accounts.basket.creator;
    let basket_id = ctx.accounts.basket.basket_id;
    let bump = [ctx.accounts.basket.bump];
    let seeds: [&[u8]; 4] = [b"basket", creator.as_ref(), basket_id.as_ref(), &bump];
    for i in 0..count {
        let c = ctx.accounts.basket.constituents[i];
        let vault_ata = &ctx.remaining_accounts[i * 4];
        let burn_ata = &ctx.remaining_accounts[i * 4 + 1];
        let mint_ai = &ctx.remaining_accounts[i * 4 + 2];
        let token_program = &ctx.remaining_accounts[i * 4 + 3];
        require_keys_eq!(*mint_ai.key, c.mint, VaultError::ConstituentMismatch);
        require_keys_eq!(*token_program.key, *mint_ai.owner, VaultError::ConstituentMismatch);
        let vault_view = read_vault(vault_ata, &basket_key, &c.mint)?;
        let burn_view = read_token_account(burn_ata)?;
        require_keys_eq!(burn_view.mint, c.mint, VaultError::ConstituentMismatch);
        require_keys_eq!(burn_view.owner, burn_key, VaultError::ConstituentMismatch);

        // As redeem_in_kind: a slice of what the vault holds, not of stored units.
        let amount = Basket::slice_of(vault_view.amount, shares, supply_before, false)?;
        require!(vault_view.amount >= amount, VaultError::VaultUndercollateralised);
        transfer_from_vault(
            mint_ai,
            vault_ata,
            burn_ata,
            &ctx.accounts.basket.to_account_info(),
            token_program,
            amount,
            mint_decimals(mint_ai)?,
            &[&seeds],
        )?;
    }
    crate::instructions::mint_redeem::resync_units(&mut ctx.accounts.basket, ctx.remaining_accounts, 4, &basket_key, supply_before - shares)
}

/// First half of a buyback slice: lends `sell_amount` of a token the burn PDA holds to
/// the rebalancer, who swaps it for GRAVI in the same transaction. The matching
/// `buyback_finish` must follow and checks at least the oracle value, less
/// `max_slippage_bps`, came back as GRAVI. `refund` more of the sold token goes to the
/// rebalancer for the buyback's network costs, as in a rebalance slice.
pub fn buyback_start(ctx: Context<BuybackStart>, sell_amount: u64, max_slippage_bps: u16, refund: u64) -> Result<()> {
    require!(sell_amount > 0, VaultError::BadParameter);
    require!(max_slippage_bps <= MAX_BUYBACK_SLIPPAGE_BPS, VaultError::BadParameter);
    require!(!ctx.accounts.buyback.pending, VaultError::BuybackPending);
    let gravi_mint = ctx.accounts.protocol.gravi_mint;
    require!(gravi_mint != Pubkey::default(), VaultError::GraviMintNotSet);
    require_keys_neq!(ctx.accounts.sell_mint.key(), gravi_mint, VaultError::BadParameter);

    let sell_mint = ctx.accounts.sell_mint.to_account_info();
    let min_gravi = oracle_floor(sell_amount, &ctx.accounts.sell_price, &ctx.accounts.gravi_price, max_slippage_bps, refund, &sell_mint)?;
    finish_follows(&ctx.accounts.instructions, <crate::instruction::BuybackFinish as anchor_lang::Discriminator>::DISCRIMINATOR)?;
    lend(
        &ctx.accounts.sell_token_program,
        &ctx.accounts.sell_source.to_account_info(),
        &sell_mint,
        &ctx.accounts.rebalancer_sell,
        &ctx.accounts.burn_authority,
        ctx.bumps.burn_authority,
        sell_amount.checked_add(refund).ok_or(VaultError::MathOverflow)?,
        ctx.accounts.sell_mint.decimals,
    )?;

    // Start and finish must read the same account: the burn PDA's canonical one.
    read_vault(&ctx.accounts.burn_gravi.to_account_info(), &ctx.accounts.burn_authority.key(), &gravi_mint)?;

    let b = &mut ctx.accounts.buyback;
    b.bump = ctx.bumps.buyback;
    b.pending = true;
    b.pending_slot = Clock::get()?.slot;
    b.min_gravi = min_gravi;
    b.gravi_before = ctx.accounts.burn_gravi.amount;
    Ok(())
}

/// First half of a sale to SOL: lends `sell_amount` of a token the burn PDA holds to the
/// rebalancer, who swaps it for wrapped SOL delivered to the burn PDA's own SOL account in the
/// same transaction. The matching `buyback_sol_finish` must follow and checks at least the oracle
/// value, less `max_slippage_bps`, came back. The SOL waits there until it all buys GRAVI in one
/// go (`buyback_start` selling SOL). `refund`: as in `buyback_start`.
pub fn buyback_sol_start(ctx: Context<BuybackSolStart>, sell_amount: u64, max_slippage_bps: u16, refund: u64) -> Result<()> {
    require!(sell_amount > 0, VaultError::BadParameter);
    require!(max_slippage_bps <= MAX_BUYBACK_SLIPPAGE_BPS, VaultError::BadParameter);
    require!(!ctx.accounts.buyback_sol.pending, VaultError::BuybackPending);

    let sell_mint = ctx.accounts.sell_mint.to_account_info();
    let min_sol = oracle_floor(sell_amount, &ctx.accounts.sell_price, &ctx.accounts.sol_price, max_slippage_bps, refund, &sell_mint)?;
    finish_follows(&ctx.accounts.instructions, <crate::instruction::BuybackSolFinish as anchor_lang::Discriminator>::DISCRIMINATOR)?;
    lend(
        &ctx.accounts.sell_token_program,
        &ctx.accounts.sell_source.to_account_info(),
        &sell_mint,
        &ctx.accounts.rebalancer_sell,
        &ctx.accounts.burn_authority,
        ctx.bumps.burn_authority,
        sell_amount.checked_add(refund).ok_or(VaultError::MathOverflow)?,
        ctx.accounts.sell_mint.decimals,
    )?;

    let held = read_vault(&ctx.accounts.burn_sol.to_account_info(), &ctx.accounts.burn_authority.key(), &NATIVE_MINT)?.amount;
    let b = &mut ctx.accounts.buyback_sol;
    b.bump = ctx.bumps.buyback_sol;
    b.pending = true;
    b.pending_slot = Clock::get()?.slot;
    b.min_sol = min_sol;
    b.sol_before = held;
    Ok(())
}

/// Second half of a sale to SOL: checks the SOL that came back clears the oracle floor. It stays
/// in the burn PDA's SOL account.
pub fn buyback_sol_finish(ctx: Context<BuybackSolFinish>) -> Result<()> {
    let b = &ctx.accounts.buyback_sol;
    require!(b.pending && b.pending_slot == Clock::get()?.slot, VaultError::NoBuybackPending);
    let held = read_vault(&ctx.accounts.burn_sol.to_account_info(), &ctx.accounts.burn_authority.key(), &NATIVE_MINT)?.amount;
    let got = held.saturating_sub(b.sol_before);
    require!(got >= b.min_sol, VaultError::BelowOracleFloor);
    let b = &mut ctx.accounts.buyback_sol;
    b.pending = false;
    b.total_sol = b.total_sol.checked_add(got).ok_or(VaultError::MathOverflow)?;
    b.slices = b.slices.saturating_add(1);
    Ok(())
}

/// The least a sale of `sell_amount` may bring back, in base units of the token bought: its
/// oracle value less `max_slippage_bps`, and less the sold token's transfer fee, which isn't
/// slippage. Never zero, which would accept nothing back. Both prices must be fresh, and the
/// keeper's `refund` within MAX_BUYBACK_REFUND_E12.
fn oracle_floor(sell_amount: u64, sell_price: &AssetPrice, out_price: &AssetPrice, max_slippage_bps: u16, refund: u64, sell_mint: &AccountInfo) -> Result<u64> {
    let now = Clock::get()?.unix_timestamp;
    for p in [sell_price, out_price] {
        require!(p.price_e12 > 0 && now - p.updated_at <= MAX_PRICE_AGE_SECS, VaultError::BadPrice);
    }
    require!(token_value(refund, sell_price.price_e12, sell_price.decimals)? <= MAX_BUYBACK_REFUND_E12, VaultError::RefundTooLarge);
    let sell_fee = TransferFee::of(sell_mint)?;
    let floor_value = token_value(sell_amount, sell_price.price_e12, sell_price.decimals)?
        * (BPS_DENOM - max_slippage_bps as u128)
        / BPS_DENOM
        * (BPS_DENOM - sell_fee.bps as u128)
        / BPS_DENOM;
    let min_out = u64::try_from(
        floor_value
            .checked_mul(10u128.pow(out_price.decimals as u32))
            .ok_or(VaultError::MathOverflow)?
            / out_price.price_e12 as u128,
    )
    .map_err(|_| VaultError::MathOverflow)?;
    require!(min_out > 0, VaultError::SliceTooSmall);
    Ok(min_out)
}

/// The finish (`discriminator`) must come later in this very transaction, with nothing of this
/// program's in between: a burn redeem landing there would count the burn PDA's own tokens as
/// what the sale brought back.
fn finish_follows(instructions: &AccountInfo, discriminator: &[u8]) -> Result<()> {
    let mut i = load_current_index_checked(instructions)? as usize + 1;
    while let Ok(ix) = load_instruction_at_checked(i, instructions) {
        if ix.program_id == crate::ID {
            require!(ix.data.len() >= 8 && ix.data[..8] == *discriminator, VaultError::BuybackInterleaved);
            return Ok(());
        }
        i += 1;
    }
    err!(VaultError::BuybackFinishMissing)
}

/// Lends `amount` of a token the burn PDA holds to the rebalancer, the burn PDA signing.
#[allow(clippy::too_many_arguments)]
fn lend<'info>(
    token_program: &Interface<'info, TokenInterface>,
    from: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    to: &UncheckedAccount<'info>,
    burn_authority: &UncheckedAccount<'info>,
    burn_bump: u8,
    amount: u64,
    decimals: u8,
) -> Result<()> {
    let burn_bump = [burn_bump];
    let burn_seeds: [&[u8]; 2] = [BURN_SEED, &burn_bump];
    token_interface::transfer_checked(
        CpiContext::new_with_signer(
            token_program.to_account_info(),
            token_interface::TransferChecked { from: from.clone(), mint: mint.clone(), to: to.to_account_info(), authority: burn_authority.to_account_info() },
            &[&burn_seeds],
        ),
        amount,
        decimals,
    )
}

/// Second half of a buyback slice: checks the GRAVI that came back clears the oracle
/// floor, then burns every GRAVI the burn PDA holds.
pub fn buyback_finish(ctx: Context<BuybackFinish>) -> Result<()> {
    let b = &ctx.accounts.buyback;
    require!(b.pending && b.pending_slot == Clock::get()?.slot, VaultError::NoBuybackPending);
    read_vault(&ctx.accounts.burn_gravi.to_account_info(), &ctx.accounts.burn_authority.key(), &ctx.accounts.gravi_mint.key())?;
    let held = ctx.accounts.burn_gravi.amount;
    require!(held.saturating_sub(b.gravi_before) >= b.min_gravi, VaultError::BelowOracleFloor);

    let burn_bump = [ctx.bumps.burn_authority];
    let burn_seeds: [&[u8]; 2] = [BURN_SEED, &burn_bump];
    token_interface::burn(
        CpiContext::new_with_signer(
            ctx.accounts.gravi_token_program.to_account_info(),
            token_interface::Burn {
                mint: ctx.accounts.gravi_mint.to_account_info(),
                from: ctx.accounts.burn_gravi.to_account_info(),
                authority: ctx.accounts.burn_authority.to_account_info(),
            },
            &[&burn_seeds],
        ),
        held,
    )?;

    let b = &mut ctx.accounts.buyback;
    b.pending = false;
    b.total_burned = b.total_burned.checked_add(held).ok_or(VaultError::MathOverflow)?;
    b.slices = b.slices.saturating_add(1);
    Ok(())
}

/// Closes a token account the burn PDA owns once it's empty, and gives its rent back to the
/// rebalancer, who paid it when the redeem opened it. A buyback then costs only network
/// fees: each run opens the accounts it redeems into and closes them once sold.
///
/// Never the burn PDA's GRAVI account (start and finish read it), and never an account of a
/// share mint: a share mint's mint authority is its basket, an account this program owns, and
/// every mint and redeem pays its burn fee into that account.
pub fn buyback_close(ctx: Context<BuybackClose>) -> Result<()> {
    require!(ctx.accounts.burn_account.amount == 0, VaultError::BurnAccountNotEmpty);
    if let anchor_lang::solana_program::program_option::COption::Some(authority) = ctx.accounts.mint.mint_authority {
        let given = ctx.accounts.mint_authority.as_ref().ok_or(error!(VaultError::BadParameter))?;
        require_keys_eq!(given.key(), authority, VaultError::BadParameter);
        require!(given.owner != &crate::ID, VaultError::BurnAccountKept);
    }
    let burn_bump = [ctx.bumps.burn_authority];
    let burn_seeds: [&[u8]; 2] = [BURN_SEED, &burn_bump];
    token_interface::close_account(CpiContext::new_with_signer(
        ctx.accounts.token_program.to_account_info(),
        token_interface::CloseAccount {
            account: ctx.accounts.burn_account.to_account_info(),
            destination: ctx.accounts.rebalancer.to_account_info(),
            authority: ctx.accounts.burn_authority.to_account_info(),
        },
        &[&burn_seeds],
    ))
}

// ---------------------------------------------------------------------------- accounts

#[derive(Accounts)]
pub struct BuybackRedeem<'info> {
    #[account(has_one = share_mint)]
    pub basket: Account<'info, Basket>,
    #[account(mut)]
    pub share_mint: InterfaceAccount<'info, MintI>,
    /// CHECK: address-only PDA; signs as the owner of burn-earmarked accounts.
    #[account(seeds = [BURN_SEED], bump)]
    pub burn_authority: UncheckedAccount<'info>,
    #[account(mut, token::mint = share_mint, token::authority = burn_authority)]
    pub burn_shares: InterfaceAccount<'info, TokenAccountI>,
    pub share_token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct BuybackStart<'info> {
    #[account(mut)]
    pub rebalancer: Signer<'info>,
    #[account(seeds = [KEEPERS_SEED], bump = keepers.bump, constraint = keepers.rebalancer == rebalancer.key() @ VaultError::NotRebalancer)]
    pub keepers: Account<'info, Keepers>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol.bump)]
    pub protocol: Account<'info, Protocol>,
    #[account(init_if_needed, payer = rebalancer, space = 8 + Buyback::INIT_SPACE, seeds = [BUYBACK_SEED], bump)]
    pub buyback: Account<'info, Buyback>,
    /// CHECK: address-only PDA; signs as the owner of burn-earmarked accounts.
    #[account(seeds = [BURN_SEED], bump)]
    pub burn_authority: UncheckedAccount<'info>,
    #[account(mint::token_program = sell_token_program)]
    pub sell_mint: InterfaceAccount<'info, MintI>,
    #[account(mut, token::mint = sell_mint, token::authority = burn_authority, token::token_program = sell_token_program)]
    pub sell_source: InterfaceAccount<'info, TokenAccountI>,
    /// CHECK: receives the lent tokens; the token program validates it on transfer.
    #[account(mut)]
    pub rebalancer_sell: UncheckedAccount<'info>,
    #[account(seeds = [PRICE_SEED, sell_mint.key().as_ref()], bump = sell_price.bump)]
    pub sell_price: Account<'info, AssetPrice>,
    #[account(seeds = [PRICE_SEED, protocol.gravi_mint.as_ref()], bump = gravi_price.bump)]
    pub gravi_price: Account<'info, AssetPrice>,
    #[account(token::mint = protocol.gravi_mint, token::authority = burn_authority)]
    pub burn_gravi: InterfaceAccount<'info, TokenAccountI>,
    pub sell_token_program: Interface<'info, TokenInterface>,
    /// CHECK: the instructions sysvar, to find the matching finish.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub instructions: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct BuybackSolStart<'info> {
    #[account(mut)]
    pub rebalancer: Signer<'info>,
    #[account(seeds = [KEEPERS_SEED], bump = keepers.bump, constraint = keepers.rebalancer == rebalancer.key() @ VaultError::NotRebalancer)]
    pub keepers: Account<'info, Keepers>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol.bump)]
    pub protocol: Account<'info, Protocol>,
    #[account(init_if_needed, payer = rebalancer, space = 8 + BuybackSol::INIT_SPACE, seeds = [BUYBACK_SOL_SEED], bump)]
    pub buyback_sol: Account<'info, BuybackSol>,
    /// CHECK: address-only PDA; signs as the owner of burn-earmarked accounts.
    #[account(seeds = [BURN_SEED], bump)]
    pub burn_authority: UncheckedAccount<'info>,
    /// Not SOL itself, and never GRAVI, which the burn PDA only ever burns.
    #[account(mint::token_program = sell_token_program, constraint = sell_mint.key() != NATIVE_MINT && sell_mint.key() != protocol.gravi_mint @ VaultError::BadParameter)]
    pub sell_mint: InterfaceAccount<'info, MintI>,
    #[account(mut, token::mint = sell_mint, token::authority = burn_authority, token::token_program = sell_token_program)]
    pub sell_source: InterfaceAccount<'info, TokenAccountI>,
    /// CHECK: receives the lent tokens; the token program validates it on transfer.
    #[account(mut)]
    pub rebalancer_sell: UncheckedAccount<'info>,
    #[account(seeds = [PRICE_SEED, sell_mint.key().as_ref()], bump = sell_price.bump)]
    pub sell_price: Account<'info, AssetPrice>,
    #[account(seeds = [PRICE_SEED, NATIVE_MINT.as_ref()], bump = sol_price.bump)]
    pub sol_price: Account<'info, AssetPrice>,
    /// CHECK: the burn PDA's canonical SOL account, checked in the instruction.
    pub burn_sol: UncheckedAccount<'info>,
    pub sell_token_program: Interface<'info, TokenInterface>,
    /// CHECK: the instructions sysvar, to find the matching finish.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub instructions: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct BuybackSolFinish<'info> {
    pub rebalancer: Signer<'info>,
    #[account(seeds = [KEEPERS_SEED], bump = keepers.bump, constraint = keepers.rebalancer == rebalancer.key() @ VaultError::NotRebalancer)]
    pub keepers: Account<'info, Keepers>,
    #[account(mut, seeds = [BUYBACK_SOL_SEED], bump = buyback_sol.bump)]
    pub buyback_sol: Account<'info, BuybackSol>,
    /// CHECK: address-only PDA; signs as the owner of burn-earmarked accounts.
    #[account(seeds = [BURN_SEED], bump)]
    pub burn_authority: UncheckedAccount<'info>,
    /// CHECK: the burn PDA's canonical SOL account, checked in the instruction.
    pub burn_sol: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct BuybackClose<'info> {
    #[account(mut)]
    pub rebalancer: Signer<'info>,
    #[account(seeds = [KEEPERS_SEED], bump = keepers.bump, constraint = keepers.rebalancer == rebalancer.key() @ VaultError::NotRebalancer)]
    pub keepers: Account<'info, Keepers>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol.bump)]
    pub protocol: Account<'info, Protocol>,
    /// CHECK: address-only PDA; signs as the owner of burn-earmarked accounts.
    #[account(seeds = [BURN_SEED], bump)]
    pub burn_authority: UncheckedAccount<'info>,
    #[account(mint::token_program = token_program, constraint = mint.key() != protocol.gravi_mint @ VaultError::BurnAccountKept)]
    pub mint: InterfaceAccount<'info, MintI>,
    #[account(mut, token::mint = mint, token::authority = burn_authority, token::token_program = token_program)]
    pub burn_account: InterfaceAccount<'info, TokenAccountI>,
    /// CHECK: the mint's mint authority, required when it has one; only its key and owner are read.
    pub mint_authority: Option<UncheckedAccount<'info>>,
    pub token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct BuybackFinish<'info> {
    pub rebalancer: Signer<'info>,
    #[account(seeds = [KEEPERS_SEED], bump = keepers.bump, constraint = keepers.rebalancer == rebalancer.key() @ VaultError::NotRebalancer)]
    pub keepers: Account<'info, Keepers>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol.bump)]
    pub protocol: Account<'info, Protocol>,
    #[account(mut, seeds = [BUYBACK_SEED], bump = buyback.bump)]
    pub buyback: Account<'info, Buyback>,
    /// CHECK: address-only PDA; signs as the owner of burn-earmarked accounts.
    #[account(seeds = [BURN_SEED], bump)]
    pub burn_authority: UncheckedAccount<'info>,
    #[account(mut, address = protocol.gravi_mint, mint::token_program = gravi_token_program)]
    pub gravi_mint: InterfaceAccount<'info, MintI>,
    #[account(mut, token::mint = gravi_mint, token::authority = burn_authority, token::token_program = gravi_token_program)]
    pub burn_gravi: InterfaceAccount<'info, TokenAccountI>,
    pub gravi_token_program: Interface<'info, TokenInterface>,
}
