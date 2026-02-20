use soroban_sdk::{contract, contractimpl, token, Address, Env};

use crate::storage::*;
use crate::types::{Config, Tier, UserInfo};

#[contract]
pub struct StakingContract;

const PRECISION: i128 = 1_000_000_000;

#[contractimpl]
impl StakingContract {
    pub fn initialize(
        env: Env,
        admin: Address,
        staking_token: Address,
        reward_token: Address,
        reward_rate: i128,
    ) {
        // Prevent re-initialization
        if env.storage().instance().has(&crate::types::DataKey::Config) {
            panic!("already initialized");
        }

        let config = Config {
            admin,
            staking_token,
            reward_token,
            reward_rate,
        };
        write_config(&env, &config);
        write_last_update_time(&env, env.ledger().timestamp());
        extend_instance(&env);
    }

    pub fn set_tier(env: Env, tier_id: u32, min_amount: i128, reward_multiplier: u32) {
        let config = read_config(&env);
        config.admin.require_auth();

        let tier = Tier {
            min_amount,
            reward_multiplier,
        };
        write_tier(&env, tier_id, &tier);
        extend_instance(&env);
    }

    pub fn stake(env: Env, user: Address, amount: i128, lock_duration: u64, tier_id: u32) {
        user.require_auth();
        if amount <= 0 {
            panic!("amount must be > 0");
        }

        update_reward(&env, Some(&user));

        let config = read_config(&env);

        // Transfer staking tokens from user to contract
        let token_client = token::Client::new(&env, &config.staking_token);
        token_client.transfer(&user, &env.current_contract_address(), &amount);

        let mut user_info = read_user_info(&env, &user).unwrap_or(UserInfo {
            amount: 0,
            shares: 0,
            reward_per_token_paid: read_reward_per_token_stored(&env),
            rewards: 0,
            lock_start_time: 0,
            lock_duration: 0,
            tier_id: 0,
        });

        // Update amount
        user_info.amount += amount;

        // Verify tier
        let tier = read_tier(&env, tier_id).unwrap_or(Tier {
            min_amount: 0,
            reward_multiplier: 100,
        });
        if user_info.amount < tier.min_amount {
            panic!("insufficient amount for tier");
        }

        // Boosting for long-term stakers: extra multiplier based on duration
        // E.g., every 30 days (2,592,000s) adds 10% to multiplier
        let boost = (lock_duration as u32 / 2_592_000) * 10;
        let total_multiplier = tier.reward_multiplier + boost;

        let new_shares = (user_info.amount * total_multiplier as i128) / 100;
        let diff_shares = new_shares - user_info.shares;

        user_info.shares = new_shares;
        user_info.tier_id = tier_id;

        // Update lock if they are staking more
        user_info.lock_start_time = env.ledger().timestamp();
        user_info.lock_duration = lock_duration;

        write_user_info(&env, &user, &user_info);

        let mut total_shares = read_total_shares(&env);
        total_shares += diff_shares;
        write_total_shares(&env, total_shares);

        extend_instance(&env);
    }

    pub fn claim(env: Env, user: Address, compound: bool) {
        user.require_auth();
        update_reward(&env, Some(&user));

        let mut user_info = read_user_info(&env, &user).expect("user not found");
        let reward = user_info.rewards;

        if reward > 0 {
            user_info.rewards = 0;
            write_user_info(&env, &user, &user_info);

            let config = read_config(&env);
            let reward_token = token::Client::new(&env, &config.reward_token);

            if compound {
                // To compound, we would stake the reward. But reward token and staking token might differ.
                // Assuming they are the same for compounding to work seamlessly, or they trade them if we had a dex.
                if config.staking_token != config.reward_token {
                    panic!("cannot compound: reward token differs from staking token");
                }

                // Keep the reward in contract, just update shares and total shares
                let tier = read_tier(&env, user_info.tier_id).unwrap_or(Tier {
                    min_amount: 0,
                    reward_multiplier: 100,
                });
                let boost = (user_info.lock_duration as u32 / 2_592_000) * 10;
                let total_multiplier = tier.reward_multiplier + boost;

                user_info.amount += reward;
                let new_shares = (user_info.amount * total_multiplier as i128) / 100;
                let diff_shares = new_shares - user_info.shares;

                user_info.shares = new_shares;
                write_user_info(&env, &user, &user_info);

                let mut total_shares = read_total_shares(&env);
                total_shares += diff_shares;
                write_total_shares(&env, total_shares);
            } else {
                reward_token.transfer(&env.current_contract_address(), &user, &reward);
            }
        }
        extend_instance(&env);
    }

    pub fn unstake(env: Env, user: Address, amount: i128) {
        user.require_auth();
        if amount <= 0 {
            panic!("amount must be > 0");
        }

        update_reward(&env, Some(&user));

        let mut user_info = read_user_info(&env, &user).expect("user not found");
        if user_info.amount < amount {
            panic!("insufficient balance");
        }

        let mut actual_amount = amount;
        let current_time = env.ledger().timestamp();

        // Early withdrawal penalty
        if current_time < user_info.lock_start_time + user_info.lock_duration {
            // Apply 20% penalty
            let penalty = (amount * 20) / 100;
            actual_amount = amount - penalty;
            // Penalty remains in contract or burned, here we just don't send it to the user.
        }

        let config = read_config(&env);

        user_info.amount -= amount;

        // Re-calculate shares
        // If they drop below tier min, should degrade tier? For simplicity, keep tier multiplier on remaining or fail if below min.
        let tier = read_tier(&env, user_info.tier_id).unwrap_or(Tier {
            min_amount: 0,
            reward_multiplier: 100,
        });
        if user_info.amount > 0 && user_info.amount < tier.min_amount {
            // Drop to base multiplier
            user_info.tier_id = 0;
        }

        let new_tier = read_tier(&env, user_info.tier_id).unwrap_or(Tier {
            min_amount: 0,
            reward_multiplier: 100,
        });
        let boost = (user_info.lock_duration as u32 / 2_592_000) * 10;
        let total_multiplier = new_tier.reward_multiplier + boost;

        let new_shares = (user_info.amount * total_multiplier as i128) / 100;
        let diff_shares = user_info.shares - new_shares;
        user_info.shares = new_shares;

        write_user_info(&env, &user, &user_info);

        let mut total_shares = read_total_shares(&env);
        total_shares -= diff_shares;
        write_total_shares(&env, total_shares);

        let token_client = token::Client::new(&env, &config.staking_token);
        token_client.transfer(&env.current_contract_address(), &user, &actual_amount);
        extend_instance(&env);
    }

    pub fn slash(env: Env, user: Address, amount: i128) {
        let config = read_config(&env);
        config.admin.require_auth();

        update_reward(&env, Some(&user));

        let mut user_info = read_user_info(&env, &user).expect("user not found");
        if user_info.amount < amount {
            panic!("slash amount exceeds balance");
        }

        user_info.amount -= amount;

        let tier = read_tier(&env, user_info.tier_id).unwrap_or(Tier {
            min_amount: 0,
            reward_multiplier: 100,
        });
        if user_info.amount > 0 && user_info.amount < tier.min_amount {
            user_info.tier_id = 0;
        }

        let new_tier = read_tier(&env, user_info.tier_id).unwrap_or(Tier {
            min_amount: 0,
            reward_multiplier: 100,
        });
        let boost = (user_info.lock_duration as u32 / 2_592_000) * 10;
        let total_multiplier = new_tier.reward_multiplier + boost;

        let new_shares = (user_info.amount * total_multiplier as i128) / 100;
        let diff_shares = user_info.shares - new_shares;
        user_info.shares = new_shares;

        write_user_info(&env, &user, &user_info);

        let mut total_shares = read_total_shares(&env);
        total_shares -= diff_shares;
        write_total_shares(&env, total_shares);

        // Slashed tokens stay in contract or could be burned.
        extend_instance(&env);
    }

    pub fn emergency_withdraw(env: Env, user: Address) {
        user.require_auth();

        // Skips reward update! Just get funds out minus 20% penalty.
        let user_info = read_user_info(&env, &user).expect("user not found");
        let amount = user_info.amount;
        if amount == 0 {
            panic!("no balance");
        }

        let penalty = (amount * 20) / 100;
        let actual_amount = amount - penalty;

        let config = read_config(&env);
        let token_client = token::Client::new(&env, &config.staking_token);

        let mut total_shares = read_total_shares(&env);
        total_shares -= user_info.shares;
        write_total_shares(&env, total_shares);

        let empty_info = UserInfo {
            amount: 0,
            shares: 0,
            reward_per_token_paid: 0,
            rewards: 0,
            lock_start_time: 0,
            lock_duration: 0,
            tier_id: 0,
        };
        write_user_info(&env, &user, &empty_info);

        token_client.transfer(&env.current_contract_address(), &user, &actual_amount);
        extend_instance(&env);
    }
}

fn update_reward(env: &Env, user: Option<&Address>) {
    let config = read_config(env);
    let mut rpt_stored = read_reward_per_token_stored(env);
    let last_update_time = read_last_update_time(env);
    let current_time = env.ledger().timestamp();

    if current_time > last_update_time {
        let total_shares = read_total_shares(env);
        if total_shares > 0 {
            let time_diff = (current_time - last_update_time) as i128;
            let reward = time_diff * config.reward_rate;
            rpt_stored += (reward * PRECISION) / total_shares;
        }
        write_reward_per_token_stored(env, rpt_stored);
        write_last_update_time(env, current_time);
    }

    if let Some(u) = user {
        let mut user_info = read_user_info(env, u).unwrap_or(UserInfo {
            amount: 0,
            shares: 0,
            reward_per_token_paid: rpt_stored,
            rewards: 0,
            lock_start_time: 0,
            lock_duration: 0,
            tier_id: 0,
        });

        let pending =
            (user_info.shares * (rpt_stored - user_info.reward_per_token_paid)) / PRECISION;
        user_info.rewards += pending;
        user_info.reward_per_token_paid = rpt_stored;
        write_user_info(env, u, &user_info);
    }
}
