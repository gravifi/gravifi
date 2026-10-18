//! Valuing a basket from its vault balances and oracle prices, for rebalancing and the buyback.

use anchor_lang::prelude::*;

use crate::errors::VaultError;
use crate::state::*;
use crate::token_util::read_vault;

pub(crate) struct Row {
    pub balance: u64,
    pub price_e12: u64,
    pub decimals: u8,
    pub value: u128,
}

pub(crate) struct Book {
    pub rows: Vec<Row>,
    /// Basket value in USD x1e12.
    pub total: u128,
}

/// USD x1e12 of `amount` base units at `price_e12` per whole token.
pub(crate) fn token_value(amount: u64, price_e12: u64, decimals: u8) -> Result<u128> {
    Ok((amount as u128)
        .checked_mul(price_e12 as u128)
        .ok_or(VaultError::MathOverflow)?
        / 10u128.pow(decimals as u32))
}

/// Values every sleeve from its vault balance and a fresh oracle price.
/// `accounts` holds [vault_ata, asset_price] per constituent, in basket order.
pub(crate) fn value_book<'info>(basket: &Account<'info, Basket>, accounts: &'info [AccountInfo<'info>], now: i64) -> Result<Book> {
    let n = basket.constituent_count as usize;
    require!(accounts.len() == n * 2, VaultError::BadRemainingAccounts);
    let basket_key = basket.key();
    let mut rows = Vec::with_capacity(n);
    let mut total: u128 = 0;
    for i in 0..n {
        let c = basket.constituents[i];
        let view = read_vault(&accounts[i * 2], &basket_key, &c.mint)?;
        let price = Account::<AssetPrice>::try_from(&accounts[i * 2 + 1]).map_err(|_| error!(VaultError::BadPrice))?;
        require_keys_eq!(price.mint, c.mint, VaultError::BadPrice);
        require!(price.price_e12 > 0 && now - price.updated_at <= MAX_PRICE_AGE_SECS, VaultError::BadPrice);
        let value = token_value(view.amount, price.price_e12, price.decimals)?;
        total = total.checked_add(value).ok_or(VaultError::MathOverflow)?;
        rows.push(Row { balance: view.amount, price_e12: price.price_e12, decimals: price.decimals, value });
    }
    require!(total > 0, VaultError::ZeroSupply);
    Ok(Book { rows, total })
}

/// Each sleeve's actual weight minus its target, in bps.
pub(crate) fn drift_bps(basket: &Basket, book: &Book) -> Result<Vec<i64>> {
    Ok(book
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| (r.value * BPS_DENOM / book.total) as i64 - basket.constituents[i].target_bps as i64)
        .collect())
}
