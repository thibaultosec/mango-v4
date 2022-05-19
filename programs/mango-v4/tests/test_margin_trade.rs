#![cfg(feature = "test-bpf")]

use anchor_lang::InstructionData;
use fixed::types::I80F48;
use solana_program_test::*;
use solana_sdk::signature::Keypair;
use solana_sdk::signature::Signer;

use mango_v4::state::*;
use program_test::*;

mod program_test;

// This is an unspecific happy-case test that just runs a few instructions to check
// that they work in principle. It should be split up / renamed.
#[tokio::test]
async fn test_margin_trade() -> Result<(), BanksClientError> {
    let mut builder = TestContextBuilder::new();
    let margin_trade = builder.add_margin_trade_program();
    let context = builder.start_default().await;
    let solana = &context.solana.clone();

    let admin = &Keypair::new();
    let owner = &context.users[0].key;
    let payer = &context.users[1].key;
    let mints = &context.mints[0..1];
    let payer_mint0_account = context.users[1].token_accounts[0];
    let dust_threshold = 0.01;

    //
    // SETUP: Create a group, account, register a token (mint0)
    //

    let mango_setup::GroupWithTokens { group, tokens } = mango_setup::GroupWithTokensConfig {
        admin,
        payer,
        mints,
    }
    .create(solana)
    .await;
    let bank = tokens[0].bank;
    let vault = tokens[0].vault;

    let account = send_tx(
        solana,
        CreateAccountInstruction {
            account_num: 0,
            group,
            owner,
            payer,
        },
    )
    .await
    .unwrap()
    .account;

    //
    // TEST: Deposit funds
    //
    let deposit_amount_initial = 100;
    {
        let start_balance = solana.token_account_balance(payer_mint0_account).await;

        send_tx(
            solana,
            DepositInstruction {
                amount: deposit_amount_initial,
                account,
                token_account: payer_mint0_account,
                token_authority: payer,
            },
        )
        .await
        .unwrap();

        assert_eq!(
            solana.token_account_balance(vault).await,
            deposit_amount_initial
        );
        assert_eq!(
            solana.token_account_balance(payer_mint0_account).await,
            start_balance - deposit_amount_initial
        );
        let account_data: MangoAccount = solana.get_account(account).await;
        let bank_data: Bank = solana.get_account(bank).await;
        assert!(
            account_data.tokens.values[0].native(&bank_data)
                - I80F48::from_num(deposit_amount_initial)
                < dust_threshold
        );
        assert!(
            bank_data.native_total_deposits() - I80F48::from_num(deposit_amount_initial)
                < dust_threshold
        );
    }

    //
    // TEST: Margin trade
    //
    let withdraw_amount = 2;
    let deposit_amount = 1;
    {
        send_tx(
            solana,
            MarginTradeInstruction {
                account,
                owner,
                mango_token_bank: bank,
                mango_token_vault: vault,
                margin_trade_program_id: margin_trade.program,
                deposit_account: margin_trade.token_account.pubkey(),
                deposit_account_owner: margin_trade.token_account_owner,
                margin_trade_program_ix_cpi_data: {
                    let ix = margin_trade::instruction::MarginTrade {
                        amount_from: withdraw_amount,
                        amount_to: deposit_amount,
                        deposit_account_owner_bump_seeds: margin_trade.token_account_bump,
                    };
                    ix.data()
                },
            },
        )
        .await
        .unwrap();
    }
    assert_eq!(
        solana.token_account_balance(vault).await,
        deposit_amount_initial - withdraw_amount + deposit_amount
    );
    assert_eq!(
        solana
            .token_account_balance(margin_trade.token_account.pubkey())
            .await,
        withdraw_amount - deposit_amount
    );

    //
    // TEST: Bringing the balance to 0 deactivates the token
    //
    let deposit_amount_initial = solana.token_account_balance(vault).await;
    let margin_account_initial = solana
        .token_account_balance(margin_trade.token_account.pubkey())
        .await;
    let withdraw_amount = deposit_amount_initial;
    let deposit_amount = 0;
    {
        send_tx(
            solana,
            MarginTradeInstruction {
                account,
                owner,
                mango_token_bank: bank,
                mango_token_vault: vault,
                margin_trade_program_id: margin_trade.program,
                deposit_account: margin_trade.token_account.pubkey(),
                deposit_account_owner: margin_trade.token_account_owner,
                margin_trade_program_ix_cpi_data: {
                    let ix = margin_trade::instruction::MarginTrade {
                        amount_from: withdraw_amount,
                        amount_to: deposit_amount,
                        deposit_account_owner_bump_seeds: margin_trade.token_account_bump,
                    };
                    ix.data()
                },
            },
        )
        .await
        .unwrap();
    }
    assert_eq!(solana.token_account_balance(vault).await, 0);
    assert_eq!(
        solana
            .token_account_balance(margin_trade.token_account.pubkey())
            .await,
        margin_account_initial + withdraw_amount
    );
    // Check that position is fully deactivated
    let account_data: MangoAccount = solana.get_account(account).await;
    assert_eq!(account_data.tokens.iter_active().count(), 0);

    //
    // TEST: Activating a token via margin trade
    //
    let margin_account_initial = solana
        .token_account_balance(margin_trade.token_account.pubkey())
        .await;
    let withdraw_amount = 0;
    let deposit_amount = margin_account_initial;
    {
        send_tx(
            solana,
            MarginTradeInstruction {
                account,
                owner,
                mango_token_bank: bank,
                mango_token_vault: vault,
                margin_trade_program_id: margin_trade.program,
                deposit_account: margin_trade.token_account.pubkey(),
                deposit_account_owner: margin_trade.token_account_owner,
                margin_trade_program_ix_cpi_data: {
                    let ix = margin_trade::instruction::MarginTrade {
                        amount_from: withdraw_amount,
                        amount_to: deposit_amount,
                        deposit_account_owner_bump_seeds: margin_trade.token_account_bump,
                    };
                    ix.data()
                },
            },
        )
        .await
        .unwrap();
    }
    assert_eq!(solana.token_account_balance(vault).await, deposit_amount);
    assert_eq!(
        solana
            .token_account_balance(margin_trade.token_account.pubkey())
            .await,
        0
    );
    // Check that position is active
    let account_data: MangoAccount = solana.get_account(account).await;
    assert_eq!(account_data.tokens.iter_active().count(), 1);

    Ok(())
}
