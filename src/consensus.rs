//! # Virtual Quorum Voting (VQV) Consensus
//!
//! Implements a minerless consensus mechanism with immediate finality.
//! Transactions are validated by a virtual quorum of validators selected via
//! deterministic randomness based on transaction hash and epoch.

use crate::transaction::{Transaction, TransactionId, Address};
use crate::economics::{RewardCalculator, TokenBalance, EconomicsError};
use crate::parent_selection::DAG;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use rand::SeedableRng;
use rand::seq::SliceRandom;
use rayon::prelude::*;

/// Validator identifier
pub type ValidatorId = Address;

/// Epoch identifier (1 hour duration)
pub type Epoch = u64;

/// Vote from a validator
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Vote {
    /// Validator who cast the vote
    pub validator_id: ValidatorId,
    
    /// Transaction being voted on
    pub transaction_id: TransactionId,
    
    /// Vote result (true = approve, false = reject)
    pub approve: bool,
    
    /// Weight of the vote (based on validator stake)
    pub weight: u64,
    
    /// Timestamp of the vote
    pub timestamp: u64,
}

impl Vote {
    /// Create a new vote
    pub fn new(
        validator_id: ValidatorId,
        transaction_id: TransactionId,
        approve: bool,
        weight: u64,
    ) -> Self {
        Self {
            validator_id,
            transaction_id,
            approve,
            weight,
            timestamp: current_timestamp_ms(),
        }
    }
}

/// Validator information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Validator {
    /// Validator ID
    pub id: ValidatorId,
    
    /// Stake amount
    pub stake: u64,
    
    /// Public key
    pub public_key: Vec<u8>,
    
    /// Last seen timestamp
    pub last_seen: u64,
    
    /// Reputation score (0.0 to 1.0)
    pub reputation: f64,
}

impl Validator {
    /// Create a new validator
    pub fn new(id: ValidatorId, stake: u64, public_key: Vec<u8>) -> Self {
        Self {
            id,
            stake,
            public_key,
            last_seen: current_timestamp_ms(),
            reputation: 1.0,
        }
    }
    
    /// Get the voting weight (stake * reputation)
    pub fn voting_weight(&self) -> u64 {
        (self.stake as f64 * self.reputation) as u64
    }
    
    /// Update last seen timestamp
    pub fn update_last_seen(&mut self) {
        self.last_seen = current_timestamp_ms();
    }
    
    /// Decrease reputation (for misbehavior)
    pub fn slash_reputation(&mut self, amount: f64) {
        self.reputation = (self.reputation - amount).max(0.0);
    }
}

/// Block identifier (32-byte hash)
pub type BlockId = [u8; 32];

/// Consensus state tracking
/// This is the single source of truth for block height, reward distribution, and finality
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusState {
    /// Current block height (incremented only on consensus-confirmed blocks)
    pub current_height: u64,
    /// Set of heights that have already received rewards (prevents double-reward attacks)
    /// DEPRECATED: Use rewarded_blocks instead for fork safety
    #[serde(skip)]
    pub rewarded_heights: std::collections::HashSet<u64>,
    /// Set of block IDs that have received rewards (fork-safe, persists across reorgs)
    pub rewarded_blocks: std::collections::HashSet<BlockId>,
    /// Minimum confirmations required before reward (finality threshold)
    pub confirmation_threshold: u64,
}

impl ConsensusState {
    /// Create new consensus state
    pub fn new() -> Self {
        Self {
            current_height: 0,
            rewarded_heights: std::collections::HashSet::new(),
            rewarded_blocks: std::collections::HashSet::new(),
            confirmation_threshold: 6, // 6 confirmations for finality (Bitcoin-like)
        }
    }

    /// Get current block height
    pub fn get_height(&self) -> u64 {
        self.current_height
    }

    /// Increment block height (only called on consensus confirmation)
    /// In solo mode (no peers), this allows testing without network validation
    pub fn increment_height(&mut self) {
        self.current_height += 1;
        tracing::info!("🔗 CONSENSUS: Block height incremented to {}", self.current_height);
    }

    /// Allow solo validation mode for testing (no peers required)
    /// This enables mining rewards even when no peers are connected
    pub fn enable_solo_mode(&mut self) {
        self.confirmation_threshold = 0; // No confirmations needed in solo mode
        tracing::info!("🔓 SOLO MODE: Enabled - no peer validation required");
    }

    /// Check if reward already given for this height (DEPRECATED - use is_block_rewarded)
    pub fn is_rewarded(&self, height: u64) -> bool {
        self.rewarded_heights.contains(&height)
    }

    /// Mark height as rewarded (prevents double-reward attacks)
    pub fn mark_rewarded(&mut self, height: u64) {
        self.rewarded_heights.insert(height);
        tracing::warn!("🔒 REWARD: Height {} marked as rewarded (prevents double-reward)", height);
    }

    /// Check if block already received reward (fork-safe)
    pub fn is_block_rewarded(&self, block_id: &BlockId) -> bool {
        self.rewarded_blocks.contains(block_id)
    }

    /// Mark block as rewarded (fork-safe, persists across reorgs)
    pub fn mark_block_rewarded(&mut self, block_id: BlockId) {
        self.rewarded_blocks.insert(block_id);
        tracing::warn!("🔒 REWARD: Block {} marked as rewarded (fork-safe)", hex::encode(block_id));
    }

    /// Validate and mark reward for a block (atomic operation, fork-safe)
    /// Returns error if block already rewarded
    pub fn validate_and_mark_block_reward(&mut self, block_id: BlockId) -> Result<(), ConsensusError> {
        if self.is_block_rewarded(&block_id) {
            tracing::error!("❌ SECURITY: Attempt to reward already-rewarded block {}", hex::encode(block_id));
            return Err(ConsensusError::DoubleRewardBlock(hex::encode(block_id)));
        }
        self.mark_block_rewarded(block_id);
        Ok(())
    }

    /// Rollback reward for a block (called on fork/reorg)
    /// Removes block from rewarded set, allowing re-reward if block gets re-confirmed
    pub fn rollback_block_reward(&mut self, block_id: &BlockId) {
        if self.rewarded_blocks.remove(block_id) {
            tracing::warn!("⚠️ FORK: Rolling back reward for block {}", hex::encode(block_id));
        }
    }

    /// Check if block has enough confirmations for finality
    pub fn is_finalized(&self, block_height: u64, current_height: u64) -> bool {
        current_height.saturating_sub(block_height) >= self.confirmation_threshold
    }

    /// Set confirmation threshold
    pub fn set_confirmation_threshold(&mut self, threshold: u64) {
        self.confirmation_threshold = threshold;
        tracing::info!("🔒 FINALITY: Confirmation threshold set to {}", threshold);
    }

    /// Compute subgraph score for Heavy Subgraph Consensus
    /// score(S) = Σ (weight(tx) × reputation(validator) × depth_factor)
    /// depth_factor = 1.0 + (depth(tx) / max_depth) × 0.5
    pub fn compute_subgraph_score(&self, tip: TransactionId, dag: &DAG, reputations: &HashMap<Address, f64>) -> f64 {
        let mut visited = HashSet::new();
        let mut total_score = 0.0;
        let max_depth = 1000.0;

        self.compute_subgraph_score_recursive(tip, dag, reputations, &mut visited, 0, &mut total_score, max_depth);
        
        tracing::debug!("📊 Subgraph score for tip {}: {}", hex::encode(&tip[..8]), total_score);
        total_score
    }

    /// Recursive helper for computing subgraph score
    fn compute_subgraph_score_recursive(
        &self,
        tx_id: TransactionId,
        dag: &DAG,
        reputations: &HashMap<Address, f64>,
        visited: &mut HashSet<TransactionId>,
        depth: u64,
        total_score: &mut f64,
        max_depth: f64,
    ) {
        if visited.contains(&tx_id) || depth > 1000 {
            return;
        }

        visited.insert(tx_id);

        if let Some(tx) = dag.get_transaction(tx_id) {
            let reputation = reputations.get(&tx.sender).copied().unwrap_or(0.5);
            let depth_factor = 1.0 + (depth as f64 / max_depth) * 0.5;
            let tx_score = tx.weight * reputation * depth_factor;
            *total_score += tx_score;

            // Recursively process parents
            for parent in &tx.parents {
                self.compute_subgraph_score_recursive(*parent, dag, reputations, visited, depth + 1, total_score, max_depth);
            }
        }
    }

    /// Select the canonical subgraph (highest-scoring tip)
    pub fn select_canonical_subgraph(&self, dag: &DAG, reputations: &HashMap<Address, f64>) -> Option<TransactionId> {
        let tips = dag.get_random_tips(100); // Get up to 100 tips
        
        if tips.is_empty() {
            return None;
        }

        let mut best_tip = None;
        let mut best_score = 0.0;

        for tip in tips {
            let score = self.compute_subgraph_score(tip, dag, reputations);
            if score > best_score {
                best_score = score;
                best_tip = Some(tip);
            }
        }

        tracing::info!("🏆 Canonical subgraph selected: tip {} with score {}", 
            hex::encode(&best_tip.unwrap_or([0u8; 32])[..8]), best_score);
        
        best_tip
    }

    /// Compute adaptive finality probability
    /// P_finality(tx) = 1 - exp(-λ × confirmations / volatility)
    pub fn compute_finality_probability(&self, tx_id: TransactionId, dag: &DAG, volatility: f64, lambda: f64) -> f64 {
        let confirmations = self.count_confirmations(tx_id, dag);
        
        if volatility == 0.0 {
            return 1.0;
        }

        let probability = 1.0 - (-lambda * confirmations as f64 / volatility).exp();
        probability.clamp(0.0, 1.0)
    }

    /// Count confirmations (number of descendants)
    fn count_confirmations(&self, tx_id: TransactionId, dag: &DAG) -> u64 {
        let mut visited = HashSet::new();
        let mut count = 0;
        self.count_descendants_recursive(tx_id, dag, &mut visited, &mut count);
        count
    }

    /// Recursive helper for counting descendants
    fn count_descendants_recursive(&self, tx_id: TransactionId, dag: &DAG, visited: &mut HashSet<TransactionId>, count: &mut u64) {
        if visited.contains(&tx_id) {
            return;
        }

        visited.insert(tx_id);

        if let Some(child_map) = dag.children().get(&tx_id) {
            for child in child_map {
                *count += 1;
                self.count_descendants_recursive(*child, dag, visited, count);
            }
        }
    }

    /// Check if transaction is finalized using adaptive finality
    pub fn is_finalized_adaptive(&self, tx_id: TransactionId, dag: &DAG, volatility: f64, threshold: f64) -> bool {
        let probability = self.compute_finality_probability(tx_id, dag, volatility, 2.0);
        probability >= threshold
    }
}

impl Default for ConsensusState {
    fn default() -> Self {
        Self::new()
    }
}

/// Virtual Quorum Voting consensus engine
#[derive(Debug)]
pub struct VQVConsensus {
    /// Registered validators
    validators: HashMap<ValidatorId, Validator>,
    
    /// Base quorum size (number of validators to select)
    base_quorum_size: usize,
    
    /// Minimum quorum size (for low-value transactions)
    min_quorum_size: usize,
    
    /// Maximum quorum size (for high-value transactions)
    max_quorum_size: usize,
    
    /// Approval threshold (percentage of quorum weight required)
    approval_threshold: f64,
    
    /// Epoch duration in milliseconds (default: 1 hour)
    epoch_duration_ms: u64,
    
    /// Minimum stake to become a validator
    min_stake: u64,
    
    /// Maximum stake per validator (prevents centralization)
    max_stake: u64,
    
    /// Adaptive quorum threshold (transaction amount above which to use max quorum)
    adaptive_threshold: u64,
    
    /// Reward calculator for mining rewards
    reward_calculator: RewardCalculator,
    
    /// Validator token balances
    validator_balances: HashMap<ValidatorId, TokenBalance>,
    
    /// Consensus state (single source of truth for height and rewards)
    state: ConsensusState,
}

impl VQVConsensus {
    /// Create a new VQV consensus engine
    pub fn new(
        quorum_size: usize,
        approval_threshold: f64,
        min_stake: u64,
        max_stake: u64,
    ) -> Self {
        Self {
            validators: HashMap::new(),
            base_quorum_size: quorum_size,
            min_quorum_size: quorum_size / 3, // Minimum 1/3 of base
            max_quorum_size: quorum_size * 2, // Maximum 2x of base
            approval_threshold: approval_threshold.clamp(0.0, 1.0),
            epoch_duration_ms: 3_600_000, // 1 hour
            min_stake,
            max_stake,
            adaptive_threshold: 1000, // Transactions above 1000 use max quorum
            reward_calculator: RewardCalculator::new(),
            validator_balances: HashMap::new(),
            state: ConsensusState::new(),
        }
    }
    
    /// Create with default parameters
    pub fn default() -> Self {
        Self {
            validators: HashMap::new(),
            base_quorum_size: 67,
            min_quorum_size: 22,
            max_quorum_size: 134,
            approval_threshold: 0.51,
            epoch_duration_ms: 3_600_000,
            min_stake: 10_000,
            max_stake: 1_000_000,
            adaptive_threshold: 1000,
            reward_calculator: RewardCalculator::new(),
            validator_balances: HashMap::new(),
            state: ConsensusState::new(),
        }
    }
    
    /// Set adaptive quorum threshold
    pub fn set_adaptive_threshold(&mut self, threshold: u64) {
        self.adaptive_threshold = threshold;
    }
    
    /// Calculate adaptive quorum size based on transaction amount
    fn calculate_adaptive_quorum_size(&self, tx_amount: u64) -> usize {
        if tx_amount >= self.adaptive_threshold {
            self.max_quorum_size
        } else if tx_amount >= self.adaptive_threshold / 10 {
            // Medium value transactions use base quorum
            self.base_quorum_size
        } else {
            // Low value transactions use minimum quorum
            self.min_quorum_size
        }
    }
    
    /// Register a new validator
    pub fn register_validator(&mut self, validator: Validator) -> Result<(), ConsensusError> {
        if validator.stake < self.min_stake {
            return Err(ConsensusError::InsufficientStake);
        }
        
        if validator.stake > self.max_stake {
            return Err(ConsensusError::StakeTooHigh);
        }
        
        self.validators.insert(validator.id, validator);
        Ok(())
    }
    
    /// Unregister a validator
    pub fn unregister_validator(&mut self, validator_id: ValidatorId) {
        self.validators.remove(&validator_id);
    }
    
    /// Get current epoch
    pub fn current_epoch(&self) -> Epoch {
        current_timestamp_ms() / self.epoch_duration_ms
    }
    
    /// Select a virtual quorum for a transaction (adaptive based on amount)
    pub fn select_quorum(&self, tx_id: TransactionId, tx_amount: u64) -> Vec<ValidatorId> {
        let epoch = self.current_epoch();
        let seed = self.deterministic_seed(tx_id, epoch);
        
        // Calculate adaptive quorum size based on transaction amount
        let quorum_size = self.calculate_adaptive_quorum_size(tx_amount);
        
        // Get all validator IDs
        let mut validator_ids: Vec<ValidatorId> = self.validators.keys().cloned().collect();
        
        // Deterministic shuffle based on seed
        let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
        validator_ids.shuffle(&mut rng);
        
        // Select top N validators
        validator_ids
            .into_iter()
            .take(quorum_size)
            .collect()
    }
    
    /// Select a virtual quorum for a transaction (legacy method without amount)
    pub fn select_quorum_legacy(&self, tx_id: TransactionId) -> Vec<ValidatorId> {
        self.select_quorum(tx_id, self.adaptive_threshold)
    }
    
    /// Calculate total stake in the network
    pub fn total_stake(&self) -> u64 {
        self.validators.values().map(|v| v.stake).sum()
    }
    
    /// Calculate total voting weight of a quorum
    pub fn quorum_weight(&self, quorum: &[ValidatorId]) -> u64 {
        quorum
            .iter()
            .filter_map(|id| self.validators.get(id))
            .map(|v| v.voting_weight())
            .sum()
    }
    
    /// Validate a transaction with quorum voting
    pub fn validate_transaction(
        &self,
        tx: &Transaction,
        votes: &[Vote],
    ) -> Result<bool, ConsensusError> {
        let quorum = self.select_quorum(tx.id, tx.amount);
        
        if quorum.is_empty() {
            return Err(ConsensusError::NoValidators);
        }
        
        let total_weight = self.quorum_weight(&quorum);
        if total_weight == 0 {
            return Err(ConsensusError::ZeroWeight);
        }
        
        // Calculate approving weight
        let approving_weight: u64 = votes
            .iter()
            .filter(|v| v.approve && quorum.contains(&v.validator_id))
            .map(|v| v.weight)
            .sum();
        
        // Check if approval threshold is met
        let approval_ratio = approving_weight as f64 / total_weight as f64;
        Ok(approval_ratio >= self.approval_threshold)
    }
    
    /// Validate multiple transactions in parallel (batch processing)
    pub fn validate_transactions_batch(
        &self,
        transactions: &[(Transaction, Vec<Vote>)],
    ) -> Result<Vec<bool>, ConsensusError> {
        transactions
            .par_iter()
            .map(|(tx, votes)| self.validate_transaction(tx, votes))
            .collect()
    }
    
    /// Select quorums for multiple transactions in parallel
    pub fn select_quorums_batch(&self, transactions: &[(TransactionId, u64)]) -> Vec<Vec<ValidatorId>> {
        transactions
            .par_iter()
            .map(|(tx_id, amount)| self.select_quorum(*tx_id, *amount))
            .collect()
    }
    
    /// Simulate voting (for testing)
    pub fn simulate_vote(&self, tx_id: TransactionId, tx_amount: u64, approve: bool) -> Vote {
        let quorum = self.select_quorum(tx_id, tx_amount);
        
        if let Some(validator_id) = quorum.first() {
            if let Some(validator) = self.validators.get(validator_id) {
                return Vote::new(
                    *validator_id,
                    tx_id,
                    approve,
                    validator.voting_weight(),
                );
            }
        }
        
        // Fallback
        Vote::new([0u8; 32], tx_id, approve, 0)
    }
    
    /// Distribute mining reward to validator
    pub fn distribute_mining_reward(&mut self, validator_id: ValidatorId, reward: u64) -> Result<(), EconomicsError> {
        // Get or create balance for validator
        let balance = self.validator_balances
            .entry(validator_id)
            .or_insert_with(|| TokenBalance::new(validator_id));
        
        balance.add_mining_reward(reward);
        
        // Update emission curve
        self.reward_calculator.update_emission(reward)?;
        
        Ok(())
    }
    
    /// Get validator token balance
    pub fn get_validator_balance(&self, validator_id: ValidatorId) -> Option<&TokenBalance> {
        self.validator_balances.get(&validator_id)
    }
    
    /// Get all validator balances
    pub fn get_all_balances(&self) -> &HashMap<ValidatorId, TokenBalance> {
        &self.validator_balances
    }
    
    /// Get reward calculator
    pub fn reward_calculator(&self) -> &RewardCalculator {
        &self.reward_calculator
    }
    
    /// Get mutable reward calculator
    pub fn reward_calculator_mut(&mut self) -> &mut RewardCalculator {
        &mut self.reward_calculator
    }
    
    /// Generate deterministic seed for quorum selection
    fn deterministic_seed(&self, tx_id: TransactionId, epoch: Epoch) -> u64 {
        
        
        let mut hasher = blake3::Hasher::new();
        hasher.update(&tx_id);
        hasher.update(&epoch.to_le_bytes());
        
        let hash = hasher.finalize();
        let bytes = hash.as_bytes();
        
        // Convert first 8 bytes to u64
        u64::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7]])
    }
    
    /// Get number of registered validators
    pub fn validator_count(&self) -> usize {
        self.validators.len()
    }
    
    /// Get a validator by ID
    pub fn get_validator(&self, id: ValidatorId) -> Option<&Validator> {
        self.validators.get(&id)
    }
    
    /// Get all validators
    pub fn get_all_validators(&self) -> Vec<&Validator> {
        self.validators.values().collect()
    }
    
    /// Get consensus state (single source of truth for height and rewards)
    pub fn state(&self) -> &ConsensusState {
        &self.state
    }
    
    /// Get mutable consensus state
    pub fn state_mut(&mut self) -> &mut ConsensusState {
        &mut self.state
    }
    
    /// Get current block height from consensus state
    pub fn get_height(&self) -> u64 {
        self.state.get_height()
    }
    
    /// Increment block height (only called on consensus confirmation)
    pub fn increment_height(&mut self) {
        self.state.increment_height();
    }
    
    /// Validate and mark reward for a block (prevents double-reward attacks, fork-safe)
    pub fn validate_and_mark_block_reward(&mut self, block_id: BlockId) -> Result<(), ConsensusError> {
        self.state.validate_and_mark_block_reward(block_id)
    }
    
    /// Rollback reward for a block (called on fork/reorg)
    pub fn rollback_block_reward(&mut self, block_id: &BlockId) {
        self.state.rollback_block_reward(block_id)
    }
}

/// Consensus error types
#[derive(Debug, thiserror::Error)]
pub enum ConsensusError {
    #[error("Insufficient stake to become validator")]
    InsufficientStake,
    
    #[error("Stake exceeds maximum allowed")]
    StakeTooHigh,
    
    #[error("No validators registered")]
    NoValidators,
    
    #[error("Quorum has zero weight")]
    ZeroWeight,
    
    #[error("Invalid vote")]
    InvalidVote,
    
    #[error("Attempt to reward already-rewarded height {0}")]
    DoubleRewardAttempt(u64),
    
    #[error("Attempt to reward already-rewarded block {0}")]
    DoubleRewardBlock(String),
    
    #[error("Block not finalized: needs {0} more confirmations")]
    NotFinalized(u64),
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
    fn test_consensus_default() {
        let consensus = VQVConsensus::default();
        assert_eq!(consensus.base_quorum_size, 67);
        assert_eq!(consensus.approval_threshold, 0.51);
        assert_eq!(consensus.min_stake, 10_000);
    }

    #[test]
    fn test_register_validator() {
        let mut consensus = VQVConsensus::default();
        
        let validator = Validator::new([1u8; 32], 100_000, vec![2u8; 32]);
        let result = consensus.register_validator(validator.clone());
        
        assert!(result.is_ok());
        assert_eq!(consensus.validator_count(), 1);
    }

    #[test]
    fn test_register_validator_insufficient_stake() {
        let mut consensus = VQVConsensus::default();
        
        let validator = Validator::new([1u8; 32], 5_000, vec![2u8; 32]); // Below min_stake
        let result = consensus.register_validator(validator);
        
        assert!(matches!(result, Err(ConsensusError::InsufficientStake)));
    }

    #[test]
    fn test_register_validator_stake_too_high() {
        let mut consensus = VQVConsensus::default();
        
        let validator = Validator::new([1u8; 32], 2_000_000, vec![2u8; 32]); // Above max_stake
        let result = consensus.register_validator(validator);
        
        assert!(matches!(result, Err(ConsensusError::StakeTooHigh)));
    }

    #[test]
    fn test_unregister_validator() {
        let mut consensus = VQVConsensus::default();
        
        let validator = Validator::new([1u8; 32], 100_000, vec![2u8; 32]);
        consensus.register_validator(validator.clone()).unwrap();
        
        consensus.unregister_validator(validator.id);
        assert_eq!(consensus.validator_count(), 0);
    }

    #[test]
    fn test_current_epoch() {
        let consensus = VQVConsensus::default();
        let epoch = consensus.current_epoch();
        
        // Epoch should be a positive number
        assert!(epoch > 0);
    }

    #[test]
    fn test_select_quorum() {
        let mut consensus = VQVConsensus::default();
        
        // Register some validators
        for i in 0..100 {
            let mut id = [0u8; 32];
            id[0] = i as u8;
            let validator = Validator::new(id, 100_000, vec![i as u8; 32]);
            consensus.register_validator(validator).unwrap();
        }
        
        let tx_id = [1u8; 32];
        let tx_amount = 500; // Low value transaction
        let quorum = consensus.select_quorum(tx_id, tx_amount);
        
        assert_eq!(quorum.len(), 67); // Should use base quorum size
    }

    #[test]
    fn test_select_quorum_deterministic() {
        let mut consensus = VQVConsensus::default();
        
        for i in 0..100 {
            let mut id = [0u8; 32];
            id[0] = i as u8;
            let validator = Validator::new(id, 100_000, vec![i as u8; 32]);
            consensus.register_validator(validator).unwrap();
        }
        
        let tx_id = [1u8; 32];
        let tx_amount = 500;
        let quorum1 = consensus.select_quorum(tx_id, tx_amount);
        let quorum2 = consensus.select_quorum(tx_id, tx_amount);
        
        // Same transaction should produce same quorum in same epoch
        assert_eq!(quorum1, quorum2);
    }

    #[test]
    fn test_adaptive_quorum() {
        let mut consensus = VQVConsensus::default();
        
        for i in 0..100 {
            let mut id = [0u8; 32];
            id[0] = i as u8;
            let validator = Validator::new(id, 100_000, vec![i as u8; 32]);
            consensus.register_validator(validator).unwrap();
        }
        
        let tx_id = [1u8; 32];
        
        // Low value transaction
        let quorum_low = consensus.select_quorum(tx_id, 100);
        assert_eq!(quorum_low.len(), 67); // Base quorum (adaptive not yet implemented)
        
        // Medium value transaction
        let quorum_medium = consensus.select_quorum(tx_id, 500);
        assert_eq!(quorum_medium.len(), 67); // Base quorum
        
        // High value transaction
        let quorum_high = consensus.select_quorum(tx_id, 2000);
        assert_eq!(quorum_high.len(), 100); // All validators (adaptive not yet implemented)
    }

    #[test]
    fn test_total_stake() {
        let mut consensus = VQVConsensus::default();
        
        let validator1 = Validator::new([1u8; 32], 100_000, vec![2u8; 32]);
        let validator2 = Validator::new([3u8; 32], 200_000, vec![4u8; 32]);
        
        consensus.register_validator(validator1).unwrap();
        consensus.register_validator(validator2).unwrap();
        
        assert_eq!(consensus.total_stake(), 300_000);
    }

    #[test]
    fn test_quorum_weight() {
        let mut consensus = VQVConsensus::default();
        
        let validator1 = Validator::new([1u8; 32], 100_000, vec![2u8; 32]);
        let validator2 = Validator::new([3u8; 32], 200_000, vec![4u8; 32]);
        
        consensus.register_validator(validator1.clone()).unwrap();
        consensus.register_validator(validator2.clone()).unwrap();
        
        let quorum = vec![validator1.id, validator2.id];
        let weight = consensus.quorum_weight(&quorum);
        
        assert_eq!(weight, 300_000);
    }

    #[test]
    fn test_validate_transaction_approved() {
        let mut consensus = VQVConsensus::new(10, 0.51, 10_000, 1_000_000);
        
        // Register validators
        for i in 0..20 {
            let mut id = [0u8; 32];
            id[0] = i as u8;
            let validator = Validator::new(id, 100_000, vec![i as u8; 32]);
            consensus.register_validator(validator).unwrap();
        }
        
        let tx = crate::transaction::Transaction::new(
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
        
        let quorum = consensus.select_quorum(tx.id, tx.amount);
        
        // Create approving votes (60% of quorum)
        let mut votes = Vec::new();
        for (i, validator_id) in quorum.iter().enumerate() {
            let approve = i < (quorum.len() * 6 / 10); // 60% approve
            if let Some(validator) = consensus.get_validator(*validator_id) {
                votes.push(Vote::new(
                    *validator_id,
                    tx.id,
                    approve,
                    validator.voting_weight(),
                ));
            }
        }
        
        let result = consensus.validate_transaction(&tx, &votes).unwrap();
        assert!(result);
    }

    #[test]
    fn test_validate_transaction_rejected() {
        let mut consensus = VQVConsensus::new(10, 0.51, 10_000, 1_000_000);
        
        for i in 0..20 {
            let mut id = [0u8; 32];
            id[0] = i as u8;
            let validator = Validator::new(id, 100_000, vec![i as u8; 32]);
            consensus.register_validator(validator).unwrap();
        }
        
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
        
        let quorum = consensus.select_quorum(tx.id, tx.amount);
        
        // Create rejecting votes (40% approve only)
        let mut votes = Vec::new();
        for (i, validator_id) in quorum.iter().enumerate() {
            let approve = i < (quorum.len() * 4 / 10); // 40% approve
            if let Some(validator) = consensus.get_validator(*validator_id) {
                votes.push(Vote::new(
                    *validator_id,
                    tx.id,
                    approve,
                    validator.voting_weight(),
                ));
            }
        }
        
        let result = consensus.validate_transaction(&tx, &votes).unwrap();
        assert!(!result);
    }

    #[test]
    fn test_validator_voting_weight() {
        let validator = Validator::new([1u8; 32], 100_000, vec![2u8; 32]);
        assert_eq!(validator.voting_weight(), 100_000);
    }

    #[test]
    fn test_validator_slash_reputation() {
        let mut validator = Validator::new([1u8; 32], 100_000, vec![2u8; 32]);
        validator.slash_reputation(0.5);
        
        assert_eq!(validator.reputation, 0.5);
        assert_eq!(validator.voting_weight(), 50_000);
    }

    #[test]
    fn test_vote_creation() {
        let vote = Vote::new([1u8; 32], [2u8; 32], true, 1000);
        
        assert_eq!(vote.validator_id, [1u8; 32]);
        assert_eq!(vote.transaction_id, [2u8; 32]);
        assert!(vote.approve);
        assert_eq!(vote.weight, 1000);
    }
}
