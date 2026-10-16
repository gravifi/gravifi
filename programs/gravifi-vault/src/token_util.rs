use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::{invoke, invoke_signed};
use anchor_spl::token_2022::spl_token_2022;
use spl_token_2022::extension::{transfer_fee::TransferFeeConfig, transfer_hook::TransferHook, BaseStateWithExtensions, StateWithExtensions};

use crate::errors::VaultError;

/// SPL Token `TransferChecked` discriminator. Classic SPL Token and Token-2022 share
/// an identical instruction layout here, so the only thing that differs between them
/// is which program id the instruction is addressed to.
const TRANSFER_CHECKED: u8 = 12;

/// Builds a `TransferChecked` against whichever token program actually owns the mint.
///
/// A GraviFi basket routinely mixes both: xStocks equities are Token-2022 while most
/// memecoins are classic SPL. Hard-coding one program would make mixed baskets — the
/// entire point of the product — impossible, so the program id is read off the mint.
fn transfer_checked_ix(
    token_program: &Pubkey,
    source: &Pubkey,
    mint: &Pubkey,
    destination: &Pubkey,
    authority: &Pubkey,
    amount: u64,
    decimals: u8,
) -> Instruction {
    let mut data = Vec::with_capacity(10);
    data.push(TRANSFER_CHECKED);
    data.extend_from_slice(&amount.to_le_bytes());
    data.push(decimals);

    Instruction {
        program_id: *token_program,
        accounts: vec![
            AccountMeta::new(*source, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new(*destination, false),
            AccountMeta::new_readonly(*authority, true),
        ],
        data,
    }
}

pub fn transfer_from_user<'info>(
    mint: &AccountInfo<'info>,
    source: &AccountInfo<'info>,
    destination: &AccountInfo<'info>,
    authority: &AccountInfo<'info>,
    token_program: &AccountInfo<'info>,
    amount: u64,
    decimals: u8,
) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    let ix = transfer_checked_ix(
        token_program.key,
        source.key,
        mint.key,
        destination.key,
        authority.key,
        amount,
        decimals,
    );
    invoke(
        &ix,
        &[
            source.clone(),
            mint.clone(),
            destination.clone(),
            authority.clone(),
            token_program.clone(),
        ],
    )
    .map_err(Into::into)
}

pub fn transfer_from_vault<'info>(
    mint: &AccountInfo<'info>,
    source: &AccountInfo<'info>,
    destination: &AccountInfo<'info>,
    authority: &AccountInfo<'info>,
    token_program: &AccountInfo<'info>,
    amount: u64,
    decimals: u8,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    let ix = transfer_checked_ix(
        token_program.key,
        source.key,
        mint.key,
        destination.key,
        authority.key,
        amount,
        decimals,
    );
    invoke_signed(
        &ix,
        &[
            source.clone(),
            mint.clone(),
            destination.clone(),
            authority.clone(),
            token_program.clone(),
        ],
        signer_seeds,
    )
    .map_err(Into::into)
}

/// Reads `decimals` straight off the mint account, which works for both token programs
/// because the base `Mint` layout is identical and extensions are appended after it.
pub fn mint_decimals(mint: &AccountInfo) -> Result<u8> {
    let data = mint.try_borrow_data()?;
    let state = StateWithExtensions::<spl_token_2022::state::Mint>::unpack(&data)
        .map_err(|_| error!(VaultError::ConstituentMismatch))?;
    Ok(state.base.decimals)
}

/// Rejects any mint carrying an *enabled* transfer hook.
///
/// xStocks ship the TransferHook extension present but disabled. Routing a hooked
/// transfer requires resolving the hook's extra account metas and passing them through
/// every CPI; until that lands, refusing the mint outright is the only safe option —
/// a vault that silently fails to move a constituent is worse than one that will not
/// accept it in the first place.
pub fn assert_no_active_transfer_hook(mint: &AccountInfo) -> Result<()> {
    if mint.owner != &spl_token_2022::ID {
        return Ok(());
    }
    let data = mint.try_borrow_data()?;
    let state = StateWithExtensions::<spl_token_2022::state::Mint>::unpack(&data)
        .map_err(|_| error!(VaultError::ConstituentMismatch))?;

    if let Ok(hook) = state.get_extension::<TransferHook>() {
        let program_id: Option<Pubkey> = hook.program_id.into();
        require!(program_id.is_none(), VaultError::TransferHookUnsupported);
    }
    Ok(())
}

/// Token-2022's transfer fee, as it applies this epoch: every transfer of the mint credits
/// the recipient the amount less this fee (bps of the amount, up to the mint's maximum). The
/// fee authority can change it, effective two epochs later, so it's read at every use.
/// Classic SPL mints and Token-2022 mints without the extension charge nothing.
pub struct TransferFee {
    pub bps: u16,
    config: Option<TransferFeeConfig>,
    epoch: u64,
}

impl TransferFee {
    pub fn of(mint: &AccountInfo) -> Result<Self> {
        let none = TransferFee { bps: 0, config: None, epoch: 0 };
        if mint.owner != &spl_token_2022::ID {
            return Ok(none);
        }
        let data = mint.try_borrow_data()?;
        let state = StateWithExtensions::<spl_token_2022::state::Mint>::unpack(&data)
            .map_err(|_| error!(VaultError::ConstituentMismatch))?;
        let Ok(config) = state.get_extension::<TransferFeeConfig>() else { return Ok(none) };
        let epoch = Clock::get()?.epoch;
        let bps = u16::from(config.get_epoch_fee(epoch).transfer_fee_basis_points);
        Ok(TransferFee { bps, config: Some(*config), epoch })
    }

    /// What arrives when `gross` is sent.
    pub fn net(&self, gross: u64) -> Result<u64> {
        let Some(c) = &self.config else { return Ok(gross) };
        let fee = c.calculate_epoch_fee(self.epoch, gross).ok_or(VaultError::MathOverflow)?;
        Ok(gross.saturating_sub(fee))
    }

    /// What must be sent for at least `net` to arrive.
    pub fn gross(&self, net: u64) -> Result<u64> {
        let Some(c) = &self.config else { return Ok(net) };
        let fee = c.calculate_inverse_epoch_fee(self.epoch, net).ok_or(VaultError::MathOverflow)?;
        net.checked_add(fee).ok_or(VaultError::MathOverflow.into())
    }

    /// Refuses a mint whose fee is above what the protocol accepts for new money.
    pub fn assert_within_cap(&self) -> Result<()> {
        require!(self.bps <= crate::state::MAX_TRANSFER_FEE_BPS, VaultError::TransferFeeTooHigh);
        Ok(())
    }
}

/// The associated token account program: a basket's vault for each constituent is its ATA.
pub const ATA_PROGRAM_ID: Pubkey = pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");

pub struct TokenAccountView {
    pub mint: Pubkey,
    pub owner: Pubkey,
    pub amount: u64,
}

/// Reads a token account's live state.
///
/// Every balance in this program is read fresh through here and never cached in the
/// Basket account, because two Token-2022 extensions on xStocks make cached quantities
/// wrong: ScaledUiAmount changes balances on dividends and splits with no transfer
/// instruction firing at all, and PermanentDelegate lets the issuer move or burn from
/// the vault's own account.
pub fn read_token_account(ai: &AccountInfo) -> Result<TokenAccountView> {
    // Without this, any program could hand us bytes shaped like a token account.
    require!(
        ai.owner == &anchor_spl::token::ID || ai.owner == &spl_token_2022::ID,
        VaultError::ConstituentMismatch
    );
    let data = ai.try_borrow_data()?;
    let state = StateWithExtensions::<spl_token_2022::state::Account>::unpack(&data)
        .map_err(|_| error!(VaultError::ConstituentMismatch))?;
    Ok(TokenAccountView {
        mint: state.base.mint,
        owner: state.base.owner,
        amount: state.base.amount,
    })
}

/// Reads a basket's vault for `mint`, which must be the basket's associated token account.
///
/// Anyone can open more token accounts owned by the basket PDA. Accepting them as vaults
/// would let a caller point `sync_units` at an empty one (zeroing a sleeve's units, then
/// minting without paying for that sleeve) or park deposits outside the books. Pinning
/// each sleeve to one canonical address closes all of that at once.
pub fn read_vault(ai: &AccountInfo, basket: &Pubkey, mint: &Pubkey) -> Result<TokenAccountView> {
    let (expected, _) = Pubkey::find_program_address(&[basket.as_ref(), ai.owner.as_ref(), mint.as_ref()], &ATA_PROGRAM_ID);
    require_keys_eq!(*ai.key, expected, VaultError::NotBasketVault);
    let view = read_token_account(ai)?;
    require_keys_eq!(view.mint, *mint, VaultError::ConstituentMismatch);
    require_keys_eq!(view.owner, *basket, VaultError::ConstituentMismatch);
    Ok(view)
}
