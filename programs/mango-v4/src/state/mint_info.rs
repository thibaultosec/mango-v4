use anchor_lang::prelude::*;
use static_assertions::const_assert_eq;
use std::mem::size_of;

use crate::error::MangoError;

use super::TokenIndex;

pub const MAX_BANKS: usize = 6;

// This struct describes which address lookup table can be used to pass
// the accounts that are relevant for this mint. The idea is that clients
// can load this account to figure out which address maps to use when calling
// instructions that need banks/oracles for all active positions.
#[account(zero_copy)]
#[derive(Debug)]
pub struct MintInfo {
    // TODO: none of these pubkeys are needed, remove?
    pub group: Pubkey,
    pub mint: Pubkey,
    pub banks: [Pubkey; MAX_BANKS],
    pub vaults: [Pubkey; MAX_BANKS],
    pub oracle: Pubkey,
    pub address_lookup_table: Pubkey,

    pub token_index: TokenIndex,

    // describe what address map relevant accounts are found on
    pub address_lookup_table_bank_index: u8,
    pub address_lookup_table_oracle_index: u8,

    pub reserved: [u8; 4],
}
const_assert_eq!(
    size_of::<MintInfo>(),
    MAX_BANKS * 2 * 32 + 4 * 32 + 2 + 2 + 4
);
const_assert_eq!(size_of::<MintInfo>() % 8, 0);

impl MintInfo {
    // used for health purposes
    pub fn first_bank(&self) -> Pubkey {
        self.banks[0]
    }

    pub fn first_vault(&self) -> Pubkey {
        self.vaults[0]
    }

    pub fn num_banks(&self) -> usize {
        self.banks
            .iter()
            .position(|&b| b == Pubkey::default())
            .unwrap_or(MAX_BANKS)
    }

    pub fn banks(&self) -> &[Pubkey] {
        &self.banks[..self.num_banks()]
    }

    pub fn verify_banks_ais(&self, all_bank_ais: &[AccountInfo]) -> Result<()> {
        require!(
            all_bank_ais.iter().map(|ai| ai.key).eq(self.banks().iter()),
            MangoError::SomeError
        );
        Ok(())
    }
}
