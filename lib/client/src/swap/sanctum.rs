use std::collections::HashSet;
use std::str::FromStr;

use anchor_lang::{system_program, Id};
use anchor_spl::token::Token;
use anyhow::Context;
use bincode::Options;
use mango_v4::accounts_zerocopy::AccountReader;
use serde::{Deserialize, Serialize};
use solana_address_lookup_table_program::state::AddressLookupTable;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::account::Account;
use solana_sdk::{instruction::Instruction, pubkey::Pubkey, signature::Signature};
use std::time::Duration;

use crate::gpa::fetch_multiple_accounts_in_chunks;
use crate::swap::sanctum_state;
use crate::{util, MangoClient, TransactionBuilder};
use borsh::BorshDeserialize;

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct QuoteResponse {
    pub in_amount: Option<String>,
    pub out_amount: String,
    pub fee_amount: String,
    pub fee_mint: String,
    pub fee_pct: String,
    pub swap_src: String,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SwapRequest {
    pub amount: String,
    pub quoted_amount: String,
    pub input: String,
    pub mode: String,
    pub output_lst_mint: String,
    pub signer: String,
    pub swap_src: String,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SanctumSwapResponse {
    pub tx: String,
}

pub struct Sanctum<'a> {
    pub mango_client: &'a MangoClient,
    pub timeout_duration: Duration,
}

impl<'a> Sanctum<'a> {
    pub async fn quote(
        &self,
        input_mint: Pubkey,
        output_mint: Pubkey,
        amount: u64,
    ) -> anyhow::Result<QuoteResponse> {
        if input_mint == output_mint {
            anyhow::bail!("Need two distinct mint to swap");
        }

        let mut account = self.mango_client.mango_account().await?;
        let input_token_index = self
            .mango_client
            .context
            .token_by_mint(&input_mint)?
            .token_index;
        let output_token_index = self
            .mango_client
            .context
            .token_by_mint(&output_mint)?
            .token_index;
        account.ensure_token_position(input_token_index)?;
        account.ensure_token_position(output_token_index)?;

        let query_args = vec![
            ("input", input_mint.to_string()),
            ("outputLstMint", output_mint.to_string()),
            ("amount", format!("{}", amount)),
        ];
        let config = self.mango_client.client.config();

        let response = self
            .mango_client
            .http_client
            .get(format!("{}/swap/quote", config.sanctum_url))
            .query(&query_args)
            .timeout(self.timeout_duration)
            .send()
            .await
            .context("quote request to sanctum")?;
        let quote: QuoteResponse =
            util::http_error_handling(response).await.with_context(|| {
                format!("error requesting sanctum route between {input_mint} and {output_mint} (using url: {})", config.sanctum_url)
            })?;

        Ok(quote)
    }

    /// Find the instructions and account lookup tables for a sanctum swap through mango
    pub async fn prepare_swap_transaction(
        &self,
        input_mint: Pubkey,
        output_mint: Pubkey,
        max_slippage_bps: u64,
        quote: &QuoteResponse,
    ) -> anyhow::Result<TransactionBuilder> {
        tracing::info!("swapping using sanctum");

        let source_token = self.mango_client.context.token_by_mint(&input_mint)?;
        let target_token = self.mango_client.context.token_by_mint(&output_mint)?;

        let bank_ams = [source_token.first_bank(), target_token.first_bank()]
            .into_iter()
            .map(util::to_writable_account_meta)
            .collect::<Vec<_>>();

        let vault_ams = [source_token.first_vault(), target_token.first_vault()]
            .into_iter()
            .map(util::to_writable_account_meta)
            .collect::<Vec<_>>();

        let owner = self.mango_client.owner();
        let account = &self.mango_client.mango_account().await?;

        let token_ams = [source_token.mint, target_token.mint]
            .into_iter()
            .map(|mint| {
                util::to_writable_account_meta(
                    anchor_spl::associated_token::get_associated_token_address(&owner, &mint),
                )
            })
            .collect::<Vec<_>>();

        let source_loan = quote
            .in_amount
            .as_ref()
            .map(|v| u64::from_str(v).unwrap())
            .unwrap_or(0);
        let loan_amounts = vec![source_loan, 0u64];
        let num_loans: u8 = loan_amounts.len().try_into().unwrap();

        // This relies on the fact that health account banks will be identical to the first_bank above!
        let (health_ams, _health_cu) = self
            .mango_client
            .derive_health_check_remaining_account_metas(
                account,
                vec![source_token.token_index, target_token.token_index],
                vec![source_token.token_index, target_token.token_index],
                vec![],
            )
            .await
            .context("building health accounts")?;

        let config = self.mango_client.client.config();

        let in_amount = quote
            .in_amount
            .clone()
            .expect("sanctum require a in amount");
        let quote_amount_u64 = quote.out_amount.parse::<u64>()?;
        let out_amount = ((quote_amount_u64 as f64) * (1.0 - (max_slippage_bps as f64) / 10_000.0))
            .ceil() as u64;

        let swap_response = self
            .mango_client
            .http_client
            .post(format!("{}/swap", config.sanctum_url))
            .json(&SwapRequest {
                amount: in_amount.clone(),
                quoted_amount: out_amount.to_string(),
                input: input_mint.to_string(),
                mode: "ExactIn".to_string(),
                output_lst_mint: output_mint.to_string(),
                signer: owner.to_string(),
                swap_src: quote.swap_src.clone(),
            })
            .timeout(self.timeout_duration)
            .send()
            .await
            .context("swap transaction request to sanctum")?;

        let swap_r: SanctumSwapResponse = util::http_error_handling(swap_response)
            .await
            .context("error requesting sanctum swap")?;

        let tx = bincode::options()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize::<solana_sdk::transaction::VersionedTransaction>(
                &base64::decode(&swap_r.tx).context("base64 decoding sanctum transaction")?,
            )
            .context("parsing sanctum transaction")?;

        let (sanctum_ixs_orig, sanctum_alts) = self
            .mango_client
            .deserialize_instructions_and_alts(&tx.message)
            .await?;

        let system_program = system_program::ID;
        let ata_program = anchor_spl::associated_token::ID;
        let token_program = anchor_spl::token::ID;
        let compute_budget_program: Pubkey = solana_sdk::compute_budget::ID;
        // these setup instructions should be placed outside of flashloan begin-end
        let is_setup_ix = |k: Pubkey| -> bool {
            k == ata_program || k == token_program || k == compute_budget_program
        };
        let sync_native_pack =
            anchor_spl::token::spl_token::instruction::TokenInstruction::SyncNative.pack();

        // Remove auto wrapping of SOL->wSOL
        let sanctum_ixs: Vec<Instruction> = sanctum_ixs_orig
            .clone()
            .into_iter()
            .filter(|ix| {
                !(ix.program_id == system_program)
                    && !(ix.program_id == token_program && ix.data == sync_native_pack)
            })
            .collect();

        let sanctum_action_ix_begin = sanctum_ixs
            .iter()
            .position(|ix| !is_setup_ix(ix.program_id))
            .ok_or_else(|| {
                anyhow::anyhow!("sanctum swap response only had setup-like instructions")
            })?;
        let sanctum_action_ix_end = sanctum_ixs.len()
            - sanctum_ixs
                .iter()
                .rev()
                .position(|ix| !is_setup_ix(ix.program_id))
                .unwrap();

        let mut instructions: Vec<Instruction> = Vec::new();

        for ix in &sanctum_ixs[..sanctum_action_ix_begin] {
            instructions.push(ix.clone());
        }

        // Ensure the source token account is created (sanctum takes care of the output account)
        instructions.push(
            spl_associated_token_account::instruction::create_associated_token_account_idempotent(
                &owner,
                &owner,
                &source_token.mint,
                &Token::id(),
            ),
        );

        instructions.push(Instruction {
            program_id: mango_v4::id(),
            accounts: {
                let mut ams = anchor_lang::ToAccountMetas::to_account_metas(
                    &mango_v4::accounts::FlashLoanBegin {
                        account: self.mango_client.mango_account_address,
                        owner,
                        token_program: Token::id(),
                        instructions: solana_sdk::sysvar::instructions::id(),
                    },
                    None,
                );
                ams.extend(bank_ams);
                ams.extend(vault_ams.clone());
                ams.extend(token_ams.clone());
                ams.push(util::to_readonly_account_meta(self.mango_client.group()));
                ams
            },
            data: anchor_lang::InstructionData::data(&mango_v4::instruction::FlashLoanBegin {
                loan_amounts,
            }),
        });

        for ix in &sanctum_ixs[sanctum_action_ix_begin..sanctum_action_ix_end] {
            instructions.push(ix.clone());
        }

        instructions.push(Instruction {
            program_id: mango_v4::id(),
            accounts: {
                let mut ams = anchor_lang::ToAccountMetas::to_account_metas(
                    &mango_v4::accounts::FlashLoanEnd {
                        account: self.mango_client.mango_account_address,
                        owner,
                        token_program: Token::id(),
                    },
                    None,
                );
                ams.extend(health_ams);
                ams.extend(vault_ams);
                ams.extend(token_ams);
                ams.push(util::to_readonly_account_meta(self.mango_client.group()));
                ams
            },
            data: anchor_lang::InstructionData::data(&mango_v4::instruction::FlashLoanEndV2 {
                num_loans,
                flash_loan_type: mango_v4::accounts_ix::FlashLoanType::Swap,
            }),
        });

        for ix in &sanctum_ixs[sanctum_action_ix_end..] {
            instructions.push(ix.clone());
        }

        let mut address_lookup_tables = self.mango_client.mango_address_lookup_tables().await?;
        address_lookup_tables.extend(sanctum_alts.into_iter());

        let payer = owner; // maybe use fee_payer? but usually it's the same

        Ok(TransactionBuilder {
            instructions,
            address_lookup_tables,
            payer,
            signers: vec![self.mango_client.owner.clone()],
            config: self
                .mango_client
                .client
                .config()
                .transaction_builder_config
                .clone(),
        })
    }

    pub async fn swap(
        &self,
        input_mint: Pubkey,
        output_mint: Pubkey,
        max_slippage_bps: u64,
        amount: u64,
    ) -> anyhow::Result<Signature> {
        let route = self.quote(input_mint, output_mint, amount).await?;

        let tx_builder = self
            .prepare_swap_transaction(input_mint, output_mint, max_slippage_bps, &route)
            .await?;

        tx_builder.send_and_confirm(&self.mango_client.client).await
    }
}

pub async fn load_supported_token_mints(
    live_rpc_client: &RpcClient,
) -> anyhow::Result<HashSet<Pubkey>> {
    let address = Pubkey::from_str("EhWxBHdmQ3yDmPzhJbKtGMM9oaZD42emt71kSieghy5")?;

    let lookup_table_data = live_rpc_client.get_account(&address).await?;
    let lookup_table = AddressLookupTable::deserialize(&lookup_table_data.data())?;
    let accounts: Vec<Account> =
        fetch_multiple_accounts_in_chunks(live_rpc_client, &lookup_table.addresses, 100, 1)
            .await?
            .into_iter()
            .map(|x| x.1)
            .collect();

    let mut lst_mints = HashSet::new();
    for account in accounts {
        let account = Account::from(account);
        let mut account_data = account.data();
        let t = sanctum_state::StakePool::deserialize(&mut account_data);
        if let Ok(d) = t {
            lst_mints.insert(d.pool_mint);
        }
    }

    // Hardcoded for now
    lst_mints.insert(
        Pubkey::from_str("CgntPoLka5pD5fesJYhGmUCF8KU1QS1ZmZiuAuMZr2az").expect("invalid lst mint"),
    );
    lst_mints.insert(
        Pubkey::from_str("7ge2xKsZXmqPxa3YmXxXmzCp9Hc2ezrTxh6PECaxCwrL").expect("invalid lst mint"),
    );
    lst_mints.insert(
        Pubkey::from_str("GUAMR8ciiaijraJeLDEDrFVaueLm9YzWWY9R7CBPL9rA").expect("invalid lst mint"),
    );
    lst_mints.insert(
        Pubkey::from_str("Jito4APyf642JPZPx3hGc6WWJ8zPKtRbRs4P815Awbb").expect("invalid lst mint"),
    );
    lst_mints.insert(
        Pubkey::from_str("CtMyWsrUtAwXWiGr9WjHT5fC3p3fgV8cyGpLTo2LJzG1").expect("invalid lst mint"),
    );
    lst_mints.insert(
        Pubkey::from_str("2qyEeSAWKfU18AFthrF7JA8z8ZCi1yt76Tqs917vwQTV").expect("invalid lst mint"),
    );
    lst_mints.insert(
        Pubkey::from_str("DqhH94PjkZsjAqEze2BEkWhFQJ6EyU6MdtMphMgnXqeK").expect("invalid lst mint"),
    );
    lst_mints.insert(
        Pubkey::from_str("F8h46pYkaqPJNP2MRkUUUtRkf8efCkpoqehn9g1bTTm7").expect("invalid lst mint"),
    );
    lst_mints.insert(
        Pubkey::from_str("5oc4nmbNTda9fx8Tw57ShLD132aqDK65vuHH4RU1K4LZ").expect("invalid lst mint"),
    );
    lst_mints.insert(
        Pubkey::from_str("stk9ApL5HeVAwPLr3TLhDXdZS8ptVu7zp6ov8HFDuMi").expect("invalid lst mint"),
    );

    Ok(lst_mints)
}
