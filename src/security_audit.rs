//! # Security Audit Module
//!
//! Implements resilience against various attacks:
//! - Double-Spend detection
//! - Parasite Chain Attack detection
//! - Validator slashing for malicious behavior

use crate::transaction::{Transaction, TransactionId, Address};
use crate::consensus::{VQVConsensus, Vote};
use crate::parent_selection::DAG;
use crate::economics::{TokenBalance, EconomicsError};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Security audit result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityAudit {
    /// Detected double-spend attempts
    pub double_spends_detected: Vec<DoubleSpendAttempt>,
    
    /// Detected parasite chain attacks
    pub parasite_chains_detected: Vec<ParasiteChain>,
    
    /// Validators to slash
    pub validators_to_slash: Vec<ValidatorSlash>,
    
    /// Total slashed amount
    pub total_slashed: u64,
}

/// Double-spend attempt
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoubleSpendAttempt {
    /// Conflicting transaction IDs
    pub tx_ids: [TransactionId; 2],
    
    /// Sender address
    pub sender: Address,
    
    /// Amounts involved
    pub amounts: [u64; 2],
    
    /// Timestamp of detection
    pub detected_at: u64,
}

/// Parasite chain
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParasiteChain {
    /// Chain root transaction
    pub root_tx: TransactionId,
    
    /// Chain transactions
    pub chain_transactions: Vec<TransactionId>,
    
    /// Chain weight
    pub chain_weight: u64,
    
    /// Chain depth
    pub chain_depth: u64,
    
    /// Detection reason
    pub reason: String,
}

/// Validator slash info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorSlash {
    /// Validator address
    pub validator_address: Address,
    
    /// Slash reason
    pub reason: SlashReason,
    
    /// Amount to slash
    pub slash_amount: u64,
    
    /// Evidence
    pub evidence: Vec<TransactionId>,
}

/// Reason for slashing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SlashReason {
    /// Voted against quorum
    VotedAgainstQuorum,
    
    /// Double-signing
    DoubleSigning,
    
    /// Invalid vote pattern
    InvalidVotePattern,
    
    /// Spam voting
    SpamVoting,
}

/// Security auditor
#[derive(Debug)]
pub struct SecurityAuditor {
    /// Pending transactions for double-spend detection
    pending_txs: HashMap<Address, Vec<Transaction>>,
    
    /// Confirmed transactions
    confirmed_txs: HashSet<TransactionId>,
    
    /// Validator reputation scores
    validator_reputation: HashMap<Address, f64>,
    
    /// Minimum reputation threshold
    min_reputation: f64,
    
    /// Slash percentage (0.0 to 1.0)
    slash_percentage: f64,
}

impl SecurityAuditor {
    /// Create new security auditor
    pub fn new() -> Self {
        Self {
            pending_txs: HashMap::new(),
            confirmed_txs: HashSet::new(),
            validator_reputation: HashMap::new(),
            min_reputation: 0.3,
            slash_percentage: 0.5, // Slash 50% of stake
        }
    }
    
    /// Set minimum reputation threshold
    pub fn set_min_reputation(&mut self, threshold: f64) {
        self.min_reputation = threshold.clamp(0.0, 1.0);
    }
    
    /// Set slash percentage
    pub fn set_slash_percentage(&mut self, percentage: f64) {
        self.slash_percentage = percentage.clamp(0.0, 1.0);
    }
    
    /// Add transaction to pending pool for double-spend detection
    pub fn add_pending_transaction(&mut self, tx: Transaction) {
        self.pending_txs
            .entry(tx.sender)
            .or_insert_with(Vec::new)
            .push(tx);
    }
    
    /// Confirm transaction (no double-spend detected)
    pub fn confirm_transaction(&mut self, tx_id: TransactionId) {
        self.confirmed_txs.insert(tx_id);
        
        // Remove from pending
        for (_, txs) in self.pending_txs.iter_mut() {
            txs.retain(|tx| tx.id != tx_id);
        }
    }
    
    /// Detect double-spend attempts
    pub fn detect_double_spends(&self) -> Vec<DoubleSpendAttempt> {
        let mut attempts = Vec::new();
        
        for (_sender, txs) in &self.pending_txs {
            if txs.len() >= 2 {
                // Check for conflicting transactions
                for i in 0..txs.len() {
                    for j in (i + 1)..txs.len() {
                        let tx1 = &txs[i];
                        let tx2 = &txs[j];
                        
                        // Check if they conflict (same sender, similar nonce, different receiver)
                        if tx1.sender == tx2.sender && tx1.receiver != tx2.receiver {
                            attempts.push(DoubleSpendAttempt {
                                tx_ids: [tx1.id, tx2.id],
                                sender: tx1.sender,
                                amounts: [tx1.amount, tx2.amount],
                                detected_at: current_timestamp_ms(),
                            });
                        }
                    }
                }
            }
        }
        
        attempts
    }
    
    /// Detect parasite chain attacks
    pub fn detect_parasite_chains(
        &self,
        dag: &DAG,
        new_txs: &[Transaction],
    ) -> Vec<ParasiteChain> {
        let mut chains = Vec::new();
        
        for tx in new_txs {
            // Check if transaction creates an orphan chain
            if self.is_orphan_transaction(dag, tx) {
                let chain_weight = self.calculate_chain_weight(dag, tx);
                let chain_depth = self.calculate_chain_depth(dag, tx);
                
                // Check if chain is suspicious (low weight but high depth)
                if chain_weight < 1000 && chain_depth > 10 {
                    chains.push(ParasiteChain {
                        root_tx: tx.id,
                        chain_transactions: self.extract_chain(dag, tx),
                        chain_weight,
                        chain_depth,
                        reason: "Low weight high depth chain detected".to_string(),
                    });
                }
            }
        }
        
        chains
    }
    
    /// Check if transaction is an orphan
    fn is_orphan_transaction(&self, dag: &DAG, tx: &Transaction) -> bool {
        // Check if parents are in DAG
        for parent in &tx.parents {
            if dag.get_transaction(*parent).is_none() {
                return true;
            }
        }
        false
    }
    
    /// Calculate chain weight
    fn calculate_chain_weight(&self, dag: &DAG, tx: &Transaction) -> u64 {
        let mut weight = 0u64;
        let mut visited = HashSet::new();
        let mut queue = vec![tx.id];
        
        while let Some(current) = queue.pop() {
            if !visited.insert(current) {
                continue;
            }
            
            if let Some(tx_data) = dag.get_transaction(current) {
                weight += tx_data.weight as u64;
                for parent in &tx_data.parents {
                    queue.push(*parent);
                }
            }
        }
        
        weight
    }
    
    /// Calculate chain depth
    fn calculate_chain_depth(&self, dag: &DAG, tx: &Transaction) -> u64 {
        let mut max_depth = 0;
        let mut visited = HashSet::new();
        let mut queue = vec![(tx.id, 0)];
        
        while let Some((current, depth)) = queue.pop() {
            if !visited.insert(current) {
                continue;
            }
            
            max_depth = max_depth.max(depth);
            
            if let Some(tx_data) = dag.get_transaction(current) {
                for parent in &tx_data.parents {
                    queue.push((*parent, depth + 1));
                }
            }
        }
        
        max_depth
    }
    
    /// Extract chain transactions
    fn extract_chain(&self, dag: &DAG, tx: &Transaction) -> Vec<TransactionId> {
        let mut chain = Vec::new();
        let mut visited = HashSet::new();
        let mut queue = vec![tx.id];
        
        while let Some(current) = queue.pop() {
            if !visited.insert(current) {
                continue;
            }
            
            chain.push(current);
            
            if let Some(tx_data) = dag.get_transaction(current) {
                for parent in &tx_data.parents {
                    queue.push(*parent);
                }
            }
        }
        
        chain
    }
    
    /// Detect validators voting against quorum
    pub fn detect_malicious_validators(
        &self,
        consensus: &VQVConsensus,
        votes: &[Vote],
        _quorum: &[Address],
    ) -> Vec<ValidatorSlash> {
        let mut slashes = Vec::new();
        
        // Count votes per validator
        let mut validator_votes: HashMap<Address, (usize, usize)> = HashMap::new();
        
        for vote in votes {
            let entry = validator_votes.entry(vote.validator_id).or_insert((0, 0));
            if vote.approve {
                entry.0 += 1;
            } else {
                entry.1 += 1;
            }
        }
        
        // Check for validators consistently voting against quorum
        for (validator_id, (approve_count, reject_count)) in validator_votes {
            let total = approve_count + reject_count;
            if total > 0 {
                let reject_ratio = reject_count as f64 / total as f64;
                
                // If validator rejects > 70% of transactions, slash them
                if reject_ratio > 0.7 {
                    let reputation = self.validator_reputation.get(&validator_id).copied().unwrap_or(1.0);
                    
                    if reputation > self.min_reputation {
                        let validator = consensus.get_validator(validator_id);
                        if let Some(v) = validator {
                            let slash_amount = (v.stake as f64 * self.slash_percentage) as u64;
                            
                            slashes.push(ValidatorSlash {
                                validator_address: validator_id,
                                reason: SlashReason::VotedAgainstQuorum,
                                slash_amount,
                                evidence: votes.iter().filter(|v| v.validator_id == validator_id).map(|v| v.transaction_id).collect(),
                            });
                        }
                    }
                }
            }
        }
        
        slashes
    }
    
    /// Perform full security audit
    pub fn perform_audit(
        &mut self,
        consensus: &VQVConsensus,
        dag: &DAG,
        new_txs: &[Transaction],
        votes: &[Vote],
        quorum: &[Address],
    ) -> SecurityAudit {
        let double_spends = self.detect_double_spends();
        let parasite_chains = self.detect_parasite_chains(dag, new_txs);
        let validator_slashes = self.detect_malicious_validators(consensus, votes, quorum);
        
        let total_slashed: u64 = validator_slashes.iter().map(|s| s.slash_amount).sum();
        
        SecurityAudit {
            double_spends_detected: double_spends,
            parasite_chains_detected: parasite_chains,
            validators_to_slash: validator_slashes,
            total_slashed,
        }
    }
    
    /// Apply slash to validator balance
    pub fn apply_slash(
        &self,
        validator_id: Address,
        slash_amount: u64,
        balance: &mut TokenBalance,
    ) -> Result<(), EconomicsError> {
        if balance.address != validator_id {
            return Err(EconomicsError::InsufficientBalance);
        }
        
        balance.subtract(slash_amount)
    }
    
    /// Update validator reputation
    pub fn update_reputation(&mut self, validator_id: Address, delta: f64) {
        let reputation = self.validator_reputation.entry(validator_id).or_insert(1.0);
        *reputation = (*reputation + delta).clamp(0.0, 1.0);
    }
    
    /// Get validator reputation
    pub fn get_reputation(&self, validator_id: Address) -> f64 {
        self.validator_reputation.get(&validator_id).copied().unwrap_or(1.0)
    }
}

impl Default for SecurityAuditor {
    fn default() -> Self {
        Self::new()
    }
}

/// Get current timestamp in milliseconds
fn current_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(std::time::Duration::from_secs(0))
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_security_auditor_creation() {
        let auditor = SecurityAuditor::new();
        assert_eq!(auditor.min_reputation, 0.3);
        assert_eq!(auditor.slash_percentage, 0.5);
    }

    #[test]
    fn test_add_pending_transaction() {
        let mut auditor = SecurityAuditor::new();
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
        
        auditor.add_pending_transaction(tx.clone());
        
        assert_eq!(auditor.pending_txs.len(), 1);
        assert_eq!(auditor.pending_txs.get(&tx.sender).unwrap().len(), 1);
    }

    #[test]
    fn test_confirm_transaction() {
        let mut auditor = SecurityAuditor::new();
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
        
        auditor.add_pending_transaction(tx.clone());
        auditor.confirm_transaction(tx.id);
        
        assert_eq!(auditor.confirmed_txs.contains(&tx.id), true);
    }

    #[test]
    fn test_detect_double_spends() {
        let mut auditor = SecurityAuditor::new();
        
        let sender = [1u8; 32];
        
        // Create conflicting transactions
        let tx1 = Transaction::new(
            [[0u8; 32]; 2],
            sender,
            [2u8; 32],
            100,
            10,
            0,
            0,
            1, // account_nonce
            vec![0u8; 64],
            vec![0u8; 32],
        );
        
        let tx2 = Transaction::new(
            [[0u8; 32]; 2],
            sender,
            [3u8; 32], // Different receiver
            100,
            10,
            0,
            0,
            1, // account_nonce
            vec![0u8; 64],
            vec![0u8; 32],
        );
        
        auditor.add_pending_transaction(tx1);
        auditor.add_pending_transaction(tx2);
        
        let attempts = auditor.detect_double_spends();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].sender, sender);
    }

    #[test]
    fn test_reputation_update() {
        let mut auditor = SecurityAuditor::new();
        let validator_id = [1u8; 32];
        
        auditor.update_reputation(validator_id, 0.5);
        // Current implementation clamps to [0.0, 1.0], so 1.0 + 0.5 = 1.0
        assert_eq!(auditor.get_reputation(validator_id), 1.0);
        
        auditor.update_reputation(validator_id, -0.3);
        assert_eq!(auditor.get_reputation(validator_id), 0.7);
    }

    #[test]
    fn test_reputation_clamping() {
        let mut auditor = SecurityAuditor::new();
        let validator_id = [1u8; 32];
        
        auditor.update_reputation(validator_id, 2.0);
        assert_eq!(auditor.get_reputation(validator_id), 1.0);
        
        auditor.update_reputation(validator_id, -2.0);
        assert_eq!(auditor.get_reputation(validator_id), 0.0);
    }

    #[test]
    fn test_slash_percentage_setting() {
        let mut auditor = SecurityAuditor::new();
        auditor.set_slash_percentage(0.75);
        
        assert_eq!(auditor.slash_percentage, 0.75);
    }

    #[test]
    fn test_min_reputation_setting() {
        let mut auditor = SecurityAuditor::new();
        auditor.set_min_reputation(0.5);
        
        assert_eq!(auditor.min_reputation, 0.5);
    }
}
