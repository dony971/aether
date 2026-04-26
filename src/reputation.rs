//! # Dynamic Reputation System
//!
//! Tracks validator reputation based on behavior.
//! Reputation influences consensus weight and transaction fees.

use crate::transaction::Address;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// Reputation score for an address
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Reputation {
    /// Current reputation score [0.0, 1.0]
    pub score: f64,
    
    /// Total stake committed
    pub stake: u64,
    
    /// Number of successful transactions
    pub successful_txs: u64,
    
    /// Number of failed/invalid transactions
    pub failed_txs: u64,
    
    /// Last activity timestamp
    pub last_activity: u64,
    
    /// Reputation decay factor (time-based)
    pub decay_factor: f64,
}

impl Reputation {
    /// Create a new reputation with initial stake
    pub fn new(initial_stake: u64) -> Self {
        Self {
            score: 0.5, // Start at neutral
            stake: initial_stake,
            successful_txs: 0,
            failed_txs: 0,
            last_activity: current_timestamp_ms(),
            decay_factor: 1.0,
        }
    }
    
    /// Update reputation after successful transaction
    pub fn update_success(&mut self, config: &ReputationConfig) {
        self.successful_txs += 1;
        self.last_activity = current_timestamp_ms();
        
        let total_txs = self.successful_txs + self.failed_txs;
        let success_rate = if total_txs > 0 {
            self.successful_txs as f64 / total_txs as f64
        } else {
            1.0
        };
        
        let stake_factor = (self.stake as f64 / 1_000_000.0).min(1.0);
        let bonus = config.reputation_bonus * success_rate * stake_factor;
        
        self.score = (self.score + bonus * (1.0 - self.score)).min(1.0);
        self.decay_factor = 1.0;
    }
    
    /// Update reputation after failed transaction
    pub fn update_failure(&mut self, config: &ReputationConfig) {
        self.failed_txs += 1;
        self.last_activity = current_timestamp_ms();
        
        let total_txs = self.successful_txs + self.failed_txs;
        let failure_rate = if total_txs > 0 {
            self.failed_txs as f64 / total_txs as f64
        } else {
            0.0
        };
        
        let penalty = config.reputation_penalty * failure_rate;
        self.score = (self.score - penalty).max(0.0);
        self.decay_factor = (self.decay_factor * 1.1).min(2.0);
    }
    
    /// Apply time-based decay
    pub fn apply_decay(&mut self, config: &ReputationConfig) {
        let now = current_timestamp_ms();
        let hours_elapsed = (now - self.last_activity) as f64 / (1000.0 * 60.0 * 60.0);
        
        if hours_elapsed > 0.0 {
            let decay = config.reputation_decay_rate * hours_elapsed * self.decay_factor;
            self.score = (self.score - decay).max(0.0);
            self.last_activity = now;
        }
    }
    
    /// Get voting weight (stake * reputation)
    pub fn voting_weight(&self) -> u64 {
        (self.stake as f64 * self.score) as u64
    }
}

/// Reputation configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReputationConfig {
    /// Reputation bonus for successful transactions
    pub reputation_bonus: f64,
    
    /// Reputation penalty for failed transactions
    pub reputation_penalty: f64,
    
    /// Time-based decay rate per hour
    pub reputation_decay_rate: f64,
    
    /// Minimum reputation to validate
    pub min_reputation: f64,
    
    /// Reputation threshold for fee discount
    pub fee_discount_threshold: f64,
    
    /// Fee discount percentage
    pub fee_discount_percentage: f64,
}

impl Default for ReputationConfig {
    fn default() -> Self {
        Self {
            reputation_bonus: 0.01,
            reputation_penalty: 0.1,
            reputation_decay_rate: 0.001,
            min_reputation: 0.1,
            fee_discount_threshold: 0.7,
            fee_discount_percentage: 0.5,
        }
    }
}

/// Reputation store
#[derive(Debug, Clone)]
pub struct ReputationStore {
    reputations: HashMap<Address, Reputation>,
    config: ReputationConfig,
}

impl ReputationStore {
    /// Create a new reputation store
    pub fn new(config: ReputationConfig) -> Self {
        Self {
            reputations: HashMap::new(),
            config,
        }
    }
    
    /// Get or create reputation for an address
    pub fn get_or_create(&mut self, address: Address, initial_stake: u64) -> &mut Reputation {
        if !self.reputations.contains_key(&address) {
            self.reputations.insert(address, Reputation::new(initial_stake));
        }
        self.reputations.get_mut(&address).unwrap()
    }
    
    /// Get reputation for an address
    pub fn get(&self, address: &Address) -> Option<&Reputation> {
        self.reputations.get(address)
    }
    
    /// Update reputation after successful transaction
    pub fn update_success(&mut self, address: Address) {
        if let Some(rep) = self.reputations.get_mut(&address) {
            rep.update_success(&self.config);
        }
    }
    
    /// Update reputation after failed transaction
    pub fn update_failure(&mut self, address: Address) {
        if let Some(rep) = self.reputations.get_mut(&address) {
            rep.update_failure(&self.config);
        }
    }
    
    /// Apply decay to all reputations
    pub fn apply_decay_all(&mut self) {
        for rep in self.reputations.values_mut() {
            rep.apply_decay(&self.config);
        }
    }
    
    /// Get all reputations
    pub fn get_all(&self) -> &HashMap<Address, Reputation> {
        &self.reputations
    }
    
    /// Check if address has minimum reputation
    pub fn has_min_reputation(&self, address: &Address) -> bool {
        self.get(address)
            .map(|r| r.score >= self.config.min_reputation)
            .unwrap_or(false)
    }
    
    /// Calculate fee discount for address
    pub fn fee_discount(&self, address: &Address) -> f64 {
        self.get(address)
            .map(|r| {
                if r.score >= self.config.fee_discount_threshold {
                    self.config.fee_discount_percentage
                } else {
                    0.0
                }
            })
            .unwrap_or(0.0)
    }
}

/// Get current timestamp in milliseconds
fn current_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_reputation_creation() {
        let rep = Reputation::new(1000);
        assert_eq!(rep.score, 0.5);
        assert_eq!(rep.stake, 1000);
    }
    
    #[test]
    fn test_reputation_success() {
        let config = ReputationConfig::default();
        let mut rep = Reputation::new(1000);
        rep.update_success(&config);
        assert!(rep.score > 0.5);
        assert_eq!(rep.successful_txs, 1);
    }
    
    #[test]
    fn test_reputation_failure() {
        let config = ReputationConfig::default();
        let mut rep = Reputation::new(1000);
        rep.update_failure(&config);
        assert!(rep.score < 0.5);
        assert_eq!(rep.failed_txs, 1);
    }
    
    #[test]
    fn test_reputation_store() {
        let config = ReputationConfig::default();
        let mut store = ReputationStore::new(config);
        let addr = [1u8; 32];
        
        store.get_or_create(addr, 1000);
        assert!(store.get(&addr).is_some());
        
        store.update_success(addr);
        assert!(store.get(&addr).unwrap().score > 0.5);
    }
}
