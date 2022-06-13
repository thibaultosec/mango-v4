use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};
use fixed::types::I80F48;
use fixed_macro::types::I80F48;

// TODO: ALTs are unavailable
//use crate::address_lookup_table;
use crate::error::*;
use crate::state::*;
use crate::util::fill16_from_str;

const INDEX_START: I80F48 = I80F48!(1_000_000);

#[derive(Accounts)]
#[instruction(token_index: TokenIndex)]
pub struct TokenRegister<'info> {
    #[account(
        has_one = admin,
    )]
    pub group: AccountLoader<'info, Group>,
    pub admin: Signer<'info>,

    pub mint: Account<'info, Mint>,

    #[account(
        init,
        // using the token_index in this seed guards against reusing it
        seeds = [group.key().as_ref(), b"Bank".as_ref(), &token_index.to_le_bytes()],
        bump,
        payer = payer,
        space = 8 + std::mem::size_of::<Bank>(),
    )]
    pub bank: AccountLoader<'info, Bank>,

    #[account(
        init,
        seeds = [group.key().as_ref(), b"Vault".as_ref(), &token_index.to_le_bytes()],
        bump,
        token::authority = group,
        token::mint = mint,
        payer = payer
    )]
    pub vault: Account<'info, TokenAccount>,

    #[account(
        init,
        // using the mint in this seed guards against registering the same mint twice
        seeds = [group.key().as_ref(), b"MintInfo".as_ref(), mint.key().as_ref()],
        bump,
        payer = payer,
        space = 8 + std::mem::size_of::<MintInfo>(),
    )]
    pub mint_info: AccountLoader<'info, MintInfo>,

    pub oracle: UncheckedAccount<'info>,

    // Creating an address lookup table needs a recent valid slot as an
    // input argument. That makes creating ALTs from governance instructions
    // impossible. Hence the ALT that this instruction uses must be created
    // externally and the admin is responsible for placing banks/oracles into
    // sensible address lookup tables.
    // constraint: must be created, have the admin authority and have free space
    // TODO: ALTs are unavailable
    //#[account(mut)]
    //pub address_lookup_table: UncheckedAccount<'info>, // TODO: wrapper?
    #[account(mut)]
    pub payer: Signer<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,

    // TODO: ALTs are unavailable
    //pub address_lookup_table_program: UncheckedAccount<'info>, // TODO: force address?
    pub rent: Sysvar<'info, Rent>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Default)]
pub struct InterestRateParams {
    pub util0: f32,
    pub rate0: f32,
    pub util1: f32,
    pub rate1: f32,
    pub max_rate: f32,
}

// TODO: should this be "configure_mint", we pass an explicit index, and allow
// overwriting config as long as the mint account stays the same?
#[allow(clippy::too_many_arguments)]
pub fn token_register(
    ctx: Context<TokenRegister>,
    token_index: TokenIndex,
    name: String,
    interest_rate_params: InterestRateParams,
    loan_fee_rate: f32,
    loan_origination_fee_rate: f32,
    maint_asset_weight: f32,
    init_asset_weight: f32,
    maint_liab_weight: f32,
    init_liab_weight: f32,
    liquidation_fee: f32,
) -> Result<()> {
    // TODO: Error if mint is already configured (technically, init of vault will fail)

    let mut bank = ctx.accounts.bank.load_init()?;
    *bank = Bank {
        name: fill16_from_str(name)?,
        group: ctx.accounts.group.key(),
        mint: ctx.accounts.mint.key(),
        vault: ctx.accounts.vault.key(),
        oracle: ctx.accounts.oracle.key(),
        deposit_index: INDEX_START,
        borrow_index: INDEX_START,
        indexed_total_deposits: I80F48::ZERO,
        indexed_total_borrows: I80F48::ZERO,
        last_updated: Clock::get()?.unix_timestamp,
        // TODO: add a require! verifying relation between the parameters
        util0: I80F48::from_num(interest_rate_params.util0),
        rate0: I80F48::from_num(interest_rate_params.rate0),
        util1: I80F48::from_num(interest_rate_params.util1),
        rate1: I80F48::from_num(interest_rate_params.rate1),
        max_rate: I80F48::from_num(interest_rate_params.max_rate),
        collected_fees_native: I80F48::ZERO,
        loan_origination_fee_rate: I80F48::from_num(loan_origination_fee_rate),
        loan_fee_rate: I80F48::from_num(loan_fee_rate),
        maint_asset_weight: I80F48::from_num(maint_asset_weight),
        init_asset_weight: I80F48::from_num(init_asset_weight),
        maint_liab_weight: I80F48::from_num(maint_liab_weight),
        init_liab_weight: I80F48::from_num(init_liab_weight),
        liquidation_fee: I80F48::from_num(liquidation_fee),
        dust: I80F48::ZERO,
        token_index,
        bump: *ctx.bumps.get("bank").ok_or(MangoError::SomeError)?,
        reserved: Default::default(),
        mint_decimals: ctx.accounts.mint.decimals,
    };

    // TODO: ALTs are unavailable
    // let alt_previous_size =
    //     address_lookup_table::addresses(&ctx.accounts.address_lookup_table.try_borrow_data()?)
    //         .len();
    let address_lookup_table = Pubkey::default();
    let alt_previous_size = 0;
    let mut mint_info = ctx.accounts.mint_info.load_init()?;
    *mint_info = MintInfo {
        group: ctx.accounts.group.key(),
        mint: ctx.accounts.mint.key(),
        bank: ctx.accounts.bank.key(),
        vault: ctx.accounts.vault.key(),
        oracle: ctx.accounts.oracle.key(),
        address_lookup_table,
        token_index,
        address_lookup_table_bank_index: alt_previous_size as u8,
        address_lookup_table_oracle_index: alt_previous_size as u8 + 1,
        reserved: Default::default(),
    };

    // TODO: ALTs are unavailable
    /*
    address_lookup_table::extend(
        ctx.accounts.address_lookup_table.to_account_info(),
        // TODO: is using the admin as ALT authority a good idea?
        ctx.accounts.admin.to_account_info(),
        ctx.accounts.payer.to_account_info(),
        &[],
        vec![ctx.accounts.bank.key(), ctx.accounts.oracle.key()],
    )?;
    */

    Ok(())
}