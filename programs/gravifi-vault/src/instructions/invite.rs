//! Invites: a wallet someone referred pays half the protocol fee for its first month, all of
//! it to the treasury. The wallet writes its own record with its first trade, paying the rent,
//! and the app's keeper co-signs it; mint and redeem read it when it's passed after the
//! constituent quads.

use anchor_lang::prelude::*;

use crate::errors::VaultError;
use crate::state::*;

/// Writes the signing wallet's invite, good until `until`, the wallet paying its rent. The
/// rebalancer signs too, so only a wallet the app says was referred gets one. Once per wallet
/// while it exists, at most `MAX_INVITE_SECS` ahead: a leaked keeper key can lower fees for a
/// month at a time, never for good, and can't move anyone's funds.
pub fn set_invite(ctx: Context<SetInvite>, until: i64) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    require!(until > now && until <= now + MAX_INVITE_SECS, VaultError::BadInvite);
    let invite = &mut ctx.accounts.invite;
    invite.bump = ctx.bumps.invite;
    invite.wallet = ctx.accounts.wallet.key();
    invite.until = until;
    Ok(())
}

/// Closes an invite that has run out, its rent back to the wallet. Anyone may: it only returns
/// the wallet its own deposit.
pub fn close_invite(ctx: Context<CloseInvite>) -> Result<()> {
    require!(ctx.accounts.invite.until <= Clock::get()?.unix_timestamp, VaultError::InviteActive);
    Ok(())
}

/// Whether `payer` trades as invited: `extra` is what follows the constituent quads, nothing
/// or one invite record. A record that has run out, or the payer's own invite address with
/// nothing written there yet, charges the full fee; anything else there is refused, so a
/// mistake never passes as a discount.
pub(crate) fn invited(extra: &[AccountInfo], payer: &Pubkey) -> Result<bool> {
    let ai = match extra {
        [] => return Ok(false),
        [ai] => ai,
        _ => return err!(VaultError::BadRemainingAccounts),
    };
    if ai.data_is_empty() && *ai.owner == anchor_lang::system_program::ID {
        let (own, _) = Pubkey::find_program_address(&[INVITE_SEED, payer.as_ref()], &crate::ID);
        require_keys_eq!(*ai.key, own, VaultError::BadInvite);
        return Ok(false);
    }
    require_keys_eq!(*ai.owner, crate::ID, VaultError::BadInvite);
    let invite = Invite::try_deserialize(&mut &ai.try_borrow_data()?[..])?;
    let expected = Pubkey::create_program_address(&[INVITE_SEED, invite.wallet.as_ref(), &[invite.bump]], &crate::ID)
        .map_err(|_| VaultError::BadInvite)?;
    require_keys_eq!(*ai.key, expected, VaultError::BadInvite);
    require_keys_eq!(invite.wallet, *payer, VaultError::BadInvite);
    Ok(Clock::get()?.unix_timestamp < invite.until)
}

#[derive(Accounts)]
pub struct SetInvite<'info> {
    #[account(mut)]
    pub wallet: Signer<'info>,
    pub rebalancer: Signer<'info>,
    #[account(seeds = [KEEPERS_SEED], bump = keepers.bump, constraint = keepers.rebalancer == rebalancer.key() @ VaultError::NotRebalancer)]
    pub keepers: Account<'info, Keepers>,
    #[account(init, payer = wallet, space = 8 + Invite::INIT_SPACE, seeds = [INVITE_SEED, wallet.key().as_ref()], bump)]
    pub invite: Account<'info, Invite>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CloseInvite<'info> {
    /// CHECK: the invite's own wallet, which gets its rent back.
    #[account(mut, address = invite.wallet @ VaultError::BadInvite)]
    pub wallet: UncheckedAccount<'info>,
    #[account(mut, close = wallet, seeds = [INVITE_SEED, invite.wallet.as_ref()], bump = invite.bump)]
    pub invite: Account<'info, Invite>,
}
