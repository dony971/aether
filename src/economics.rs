//! # Token Economics Module
//!
//! Implements fair launch token economics based on Micro-PoW mining.
//! Features:
//! - Hard cap (21 billion tokens)
//! - Decreasing emission curve (halving schedule)
//! - PoW-based reward distribution
//! - Validator incentives

use crate::transaction::{Transaction, Address};
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Total token supply cap (21 billion)
pub const HARD_CAP: u64 = 21_000_000_000;

/// Initial block reward (in tokens)
pub const INITIAL_REWARD: u64 = 50;

/// Halving interval (in blocks/epochs)
pub const HALVING_INTERVAL: u64 = 2_100_000;

/// Minimum reward (when halving reaches floor)
pub const MIN_REWARD: u64 = 1;

/// Token amount type
pub type TokenAmount = u64;

/// Emission curve parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmissionCurve {
    /// Total supply emitted so far
    pub total_emitted: TokenAmount,
    
    /// Current epoch/block number
    pub current_epoch: u64,
    
    /// Current reward per block
    pub current_reward: TokenAmount,
    
    /// Number of halvings that have occurred
    pub halvings_count: u64,
}

impl EmissionCurve {
    /// Create new emission curve
    pub fn new() -> Self {
        Self {
            total_emitted: 0,
            current_epoch: 0,
            current_reward: INITIAL_REWARD,
            halvings_count: 0,
        }
    }
    
    /// Calculate reward for current epoch based on halving schedule
    pub fn calculate_reward(&self) -> TokenAmount {
        let halvings = self.halvings_count;
        let mut reward = INITIAL_REWARD;
        
        // Apply halving
        for _ in 0..halvings {
            reward = reward / 2;
            if reward < MIN_REWARD {
                reward = MIN_REWARD;
                break;
            }
        }
        
        reward
    }
    
    /// Update emission curve after a block/epoch
    pub fn update(&mut self, tokens_emitted: TokenAmount) -> Result<(), EconomicsError> {
        // Check hard cap
        if self.total_emitted + tokens_emitted > HARD_CAP {
            return Err(EconomicsError::HardCapReached);
        }
        
        self.total_emitted += tokens_emitted;
        self.current_epoch += 1;
        
        // Check if halving is needed
        if self.current_epoch % HALVING_INTERVAL == 0 {
            self.halvings_count += 1;
            self.current_reward = self.calculate_reward();
        }
        
        Ok(())
    }
    
    /// Get emission percentage
    pub fn emission_percentage(&self) -> f64 {
        (self.total_emitted as f64 / HARD_CAP as f64) * 100.0
    }
    
    /// Get remaining supply
    pub fn remaining_supply(&self) -> TokenAmount {
        HARD_CAP - self.total_emitted
    }
    
    /// Check if hard cap is reached
    pub fn is_cap_reached(&self) -> bool {
        self.total_emitted >= HARD_CAP
    }
}

/// Mining reward calculator
#[derive(Debug, Clone)]
pub struct RewardCalculator {
    emission_curve: EmissionCurve,
    
    /// Base reward multiplier based on PoW difficulty
    difficulty_multiplier: f64,
    
    /// Minimum PoW difficulty to qualify for mining reward
    min_mining_difficulty: u64,
}

impl RewardCalculator {
    /// Create new reward calculator
    pub fn new() -> Self {
        Self {
            emission_curve: EmissionCurve::new(),
            difficulty_multiplier: 1.0,
            min_mining_difficulty: 1000, // Higher than anti-spam threshold
        }
    }
    
    /// Set difficulty multiplier
    pub fn set_difficulty_multiplier(&mut self, multiplier: f64) {
        self.difficulty_multiplier = multiplier.clamp(0.1, 10.0);
    }
    
    /// Set minimum mining difficulty
    pub fn set_min_mining_difficulty(&mut self, difficulty: u64) {
        self.min_mining_difficulty = difficulty;
    }
    
    /// Calculate mining reward for a transaction based on PoW difficulty
    pub fn calculate_mining_reward(&self, _tx: &Transaction, tx_difficulty: u64) -> TokenAmount {
        // Only reward if PoW difficulty meets minimum threshold
        if tx_difficulty < self.min_mining_difficulty {
            return 0;
        }
        
        // Base reward from emission curve
        let base_reward = self.emission_curve.calculate_reward();
        
        // Apply difficulty multiplier (higher difficulty = higher reward)
        let difficulty_factor = (tx_difficulty as f64 / self.min_mining_difficulty as f64)
            .min(5.0); // Cap at 5x multiplier
        
        let reward = (base_reward as f64 * self.difficulty_multiplier * difficulty_factor) as TokenAmount;
        
        reward
    }
    
    /// Update emission curve after reward distribution
    pub fn update_emission(&mut self, tokens_emitted: TokenAmount) -> Result<(), EconomicsError> {
        self.emission_curve.update(tokens_emitted)
    }
    
    /// Get current emission curve
    pub fn emission_curve(&self) -> &EmissionCurve {
        &self.emission_curve
    }
    
    /// Get mutable emission curve
    pub fn emission_curve_mut(&mut self) -> &mut EmissionCurve {
        &mut self.emission_curve
    }
}

impl Default for RewardCalculator {
    fn default() -> Self {
        Self::new()
    }
}

/// Token balance for an address
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBalance {
    /// Address
    pub address: Address,
    
    /// Token balance
    pub balance: TokenAmount,
    
    /// Total mining rewards earned
    pub mining_rewards: TokenAmount,
    
    /// Last update timestamp
    pub last_updated: u64,
}

impl TokenBalance {
    /// Create new token balance
    pub fn new(address: Address) -> Self {
        Self {
            address,
            balance: 0,
            mining_rewards: 0,
            last_updated: current_timestamp_ms(),
        }
    }
    
    /// Add tokens to balance
    pub fn add(&mut self, amount: TokenAmount) {
        self.balance += amount;
        self.last_updated = current_timestamp_ms();
    }
    
    /// Subtract tokens from balance
    pub fn subtract(&mut self, amount: TokenAmount) -> Result<(), EconomicsError> {
        if amount > self.balance {
            return Err(EconomicsError::InsufficientBalance);
        }
        self.balance -= amount;
        self.last_updated = current_timestamp_ms();
        Ok(())
    }
    
    /// Add mining reward
    pub fn add_mining_reward(&mut self, amount: TokenAmount) {
        self.mining_rewards += amount;
        self.add(amount);
    }
}

/// Economics error types
#[derive(Debug, thiserror::Error)]
pub enum EconomicsError {
    #[error("Hard cap reached: no more tokens can be emitted")]
    HardCapReached,
    
    #[error("Insufficient balance")]
    InsufficientBalance,
    
    #[error("Invalid reward calculation")]
    InvalidReward,
    
    #[error("Transaction amount too low for mining reward")]
    AmountTooLow,
}

/// Get current timestamp in milliseconds
fn current_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_emission_curve_initial() {
        let curve = EmissionCurve::new();
        assert_eq!(curve.total_emitted, 0);
        assert_eq!(curve.current_epoch, 0);
        assert_eq!(curve.current_reward, INITIAL_REWARD);
        assert_eq!(curve.halvings_count, 0);
    }

    #[test]
    fn test_emission_curve_update() {
        let mut curve = EmissionCurve::new();
        curve.update(INITIAL_REWARD).unwrap();
        
        assert_eq!(curve.total_emitted, INITIAL_REWARD);
        assert_eq!(curve.current_epoch, 1);
    }

    #[test]
    fn test_halving_schedule() {
        let mut curve = EmissionCurve::new();
        
        // Simulate halving
        curve.current_epoch = HALVING_INTERVAL - 1;
        curve.halvings_count = 0;
        curve.current_reward = INITIAL_REWARD;
        
        curve.update(INITIAL_REWARD).unwrap();
        
        assert_eq!(curve.halvings_count, 1);
        assert_eq!(curve.current_reward, INITIAL_REWARD / 2);
    }

    #[test]
    fn test_hard_cap_enforcement() {
        let mut curve = EmissionCurve::new();
        curve.total_emitted = HARD_CAP;
        
        let result = curve.update(1);
        assert!(matches!(result, Err(EconomicsError::HardCapReached)));
    }

    #[test]
    fn test_emission_percentage() {
        let mut curve = EmissionCurve::new();
        curve.total_emitted = HARD_CAP / 2;
        
        assert_eq!(curve.emission_percentage(), 50.0);
    }

    #[test]
    fn test_remaining_supply() {
        let mut curve = EmissionCurve::new();
        curve.total_emitted = 1_000_000;
        
        assert_eq!(curve.remaining_supply(), HARD_CAP - 1_000_000);
    }

    #[test]
    fn test_reward_calculator_initial() {
        let calculator = RewardCalculator::new();
        assert_eq!(calculator.emission_curve().current_reward, INITIAL_REWARD);
    }

    #[test]
    fn test_mining_reward_calculation() {
        let calculator = RewardCalculator::new();
        
        let tx = Transaction::new(
            [[0u8; 32]; 2],
            [1u8; 32],
            [2u8; 32],
            1000,
            10,
            0,
            0,
            1, // account_nonce
            vec![0u8; 64],
            vec![0u8; 32],
        );
        
        // High difficulty transaction
        let reward = calculator.calculate_mining_reward(&tx, 5000);
        assert!(reward > 0);
    }

    #[test]
    fn test_mining_reward_below_threshold() {
        let calculator = RewardCalculator::new();
        
        let tx = Transaction::new(
            [[0u8; 32]; 2],
            [1u8; 32],
            [2u8; 32],
            100,
            10,
            0,
            0,
            1, // account_nonce
            vec![0u8; 64],
            vec![0u8; 32],
        );
        
        // Low difficulty transaction (below minimum)
        let reward = calculator.calculate_mining_reward(&tx, 100);
        assert_eq!(reward, 0);
    }

    #[test]
    fn test_token_balance_add() {
        let mut balance = TokenBalance::new([1u8; 32]);
        balance.add(1000);
        
        assert_eq!(balance.balance, 1000);
    }

    #[test]
    fn test_token_balance_subtract() {
        let mut balance = TokenBalance::new([1u8; 32]);
        balance.add(1000);
        
        balance.subtract(500).unwrap();
        assert_eq!(balance.balance, 500);
    }

    #[test]
    fn test_token_balance_insufficient() {
        let mut balance = TokenBalance::new([1u8; 32]);
        balance.add(100);
        
        let result = balance.subtract(200);
        assert!(matches!(result, Err(EconomicsError::InsufficientBalance)));
    }

    #[test]
    fn test_mining_rewards_tracking() {
        let mut balance = TokenBalance::new([1u8; 32]);
        balance.add_mining_reward(500);
        
        assert_eq!(balance.balance, 500);
        assert_eq!(balance.mining_rewards, 500);
    }

    #[test]
    fn test_difficulty_multiplier() {
        let mut calculator = RewardCalculator::new();
        calculator.set_difficulty_multiplier(2.0);
        
        let tx = Transaction::new(
            [[0u8; 32]; 2],
            [1u8; 32],
            [2u8; 32],
            1000,
            10,
            0,
            0,
            1, // account_nonce
            vec![0u8; 64],
            vec![0u8; 32],
        );
        
        let reward1 = calculator.calculate_mining_reward(&tx, 5000);
        
        calculator.set_difficulty_multiplier(1.0);
        let reward2 = calculator.calculate_mining_reward(&tx, 5000);
        
        assert!(reward1 > reward2);
    }

    #[test]
    fn test_multiple_halvings() {
        let mut curve = EmissionCurve::new();
        
        // Simulate multiple halvings
        for _ in 0..5 {
            curve.current_epoch += HALVING_INTERVAL;
            curve.halvings_count += 1;
        }
        
        let reward = curve.calculate_reward();
        assert!(reward <= INITIAL_REWARD / 32); // 5 halvings = /32
    }

    #[test]
    fn test_min_reward_floor() {
        let mut curve = EmissionCurve::new();
        
        // Simulate many halvings to reach floor
        for _ in 0..20 {
            curve.halvings_count += 1;
        }
        
        let reward = curve.calculate_reward();
        assert_eq!(reward, MIN_REWARD);
    }
}
