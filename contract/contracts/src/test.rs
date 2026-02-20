#![cfg(test)]

use crate::contract::{StakingContract, StakingContractClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token, Address, Env,
};

fn create_token_contract<'a>(env: &Env, admin: &Address) -> token::Client<'a> {
    let contract_id = env.register_stellar_asset_contract_v2(admin.clone());
    token::Client::new(env, &contract_id.address())
}

#[test]
fn test_staking_lifecycle() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let user1 = Address::generate(&env);

    // Create token
    let token = create_token_contract(&env, &admin);
    let token_admin = token::StellarAssetClient::new(&env, &token.address);
    token_admin.mint(&user1, &1_000_000);

    let contract_id = env.register(StakingContract, ());
    let client = StakingContractClient::new(&env, &contract_id);

    // Initialize (reward rate = 1 token per second, precision 1e9 wait, config stores real amount, so 100_000_000 for 10% or just 1 for 1 token)
    // 1 token = e.g. 10^7 stroops, let's just use 10 for simplicity
    client.initialize(&admin, &token.address, &token.address, &10);

    // Set tier 1 to 100x multiplier base.
    client.set_tier(&1, &1000, &150); // > 1000 tokens => 1.5x

    // User stakes 2000 tokens, 30 day lock
    let lock_duration = 30 * 24 * 60 * 60;
    client.stake(&user1, &2000, &lock_duration, &1);

    // Initial check
    assert_eq!(token.balance(&user1), 1_000_000 - 2000);
    assert_eq!(token.balance(&contract_id), 2000);

    // Advance time by 10 seconds
    let mut ledger = env.ledger().get();
    ledger.timestamp += 10;
    env.ledger().set(ledger);

    // They should earn ~10 * 10 = 100 tokens
    // Mint tokens to the contract to pay out rewards
    token_admin.mint(&contract_id, &100_000);

    // Claim, not compounding
    client.claim(&user1, &false);

    // Reward = 10s * 10 = 100 token expected
    // So user token balance should be old (998k) + 100 = 998,100
    assert_eq!(token.balance(&user1), 998_100);

    // Advance time again, attempt to unstake before lock
    ledger = env.ledger().get();
    ledger.timestamp += 10;
    env.ledger().set(ledger);

    client.unstake(&user1, &1000);

    // Penalty for early withdraw = 20%
    // of 1000 = 200 penalty. So user gets 800 back.
    // User already had 998_100. Should now have 998_100 + 800 = 998_900.
    assert_eq!(token.balance(&user1), 998_900);

    // Slashes
    client.slash(&user1, &500);

    // Emergency withdraw the rest (500)
    client.emergency_withdraw(&user1);
    // 20% penalty on emergency withdraw = 100. User gets 400.
    // Has 998_900. Now has 998_900 + 400 = 999_300.
    assert_eq!(token.balance(&user1), 999_300);
}
