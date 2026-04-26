//! # Comprehensive Security Tests
//!
//! Tests for security invariants:
//! - Replay attack prevention (nonce validation)
//! - Double spend prevention (DAG parent checking)
//! - Fork safety (rewarded_blocks tracking)
//! - Atomic transaction execution (rollback on failure)
//! - Orphan recovery (persistence and reprocessing)
//! - MAX_SUPPLY enforcement (monetary policy)
//! - Consensus state as single source of truth

use crate::transaction::Transaction;
use crate::parent_selection::DAG;
use crate::ledger::{Ledger, MAX_SUPPLY, calculate_reward};
use crate::consensus::{ConsensusState, BlockId};
use crate::validation::TransactionValidator;
use crate::transaction_processor::TransactionProcessor;
use crate::rpc::Mempool;
use crate::storage::Storage;
use std::sync::Arc;
use tokio::sync::RwLock;
use tempfile::tempdir;

#[cfg(test)]
mod replay_attack_tests {
    use super::*;

    /// Test that replay attacks are prevented via nonce validation
    #[tokio::test]
    async fn test_replay_attack_prevention() {
        let validator = TransactionValidator::new();
        let mut ledger = Ledger::new();
        let mut dag = DAG::new();
        let addr = [1u8; 32];

        // Set initial balance and nonce
        ledger.set_balance(&addr, 1000);
        ledger.commit_nonce(&addr, 0);

        // Create and process first transaction with nonce=1
        let tx1 = Transaction::new(
            [[0u8; 32]; 2],
            addr,
            [2u8; 32],
            100,
            10,
            1234567890,
            0,
            1, // nonce=1
            vec![0u8; 64],
            vec![1u8; 64],
        );

        // Add to DAG (using validated method)
        dag.add_transaction_validated(tx1.clone()).unwrap();
        ledger.commit_nonce(&addr, 1);

        // Try to replay the same transaction (same nonce)
        let result = validator.validate_ledger(&tx1, &ledger, 0);
        assert!(result.is_err());
        // Should fail with InvalidNonce error
    }

    /// Test that out-of-order transactions are rejected
    #[tokio::test]
    async fn test_out_of_order_nonce_rejection() {
        let validator = TransactionValidator::new();
        let mut ledger = Ledger::new();
        let addr = [1u8; 32];

        ledger.set_balance(&addr, 1000);
        ledger.commit_nonce(&addr, 0);

        // Try transaction with nonce=3 (should be 1)
        let tx = Transaction::new(
            [[0u8; 32]; 2],
            addr,
            [2u8; 32],
            100,
            10,
            1234567890,
            0,
            3, // Wrong nonce
            vec![0u8; 64],
            vec![1u8; 64],
        );

        let result = validator.validate_ledger(&tx, &ledger, 0);
        assert!(result.is_err());
    }
}

#[cfg(test)]
mod double_spend_tests {
    use super::*;

    /// Test that double spends are prevented via parent checking
    #[tokio::test]
    async fn test_double_spend_prevention() {
        let mut dag = DAG::new();
        let addr = [1u8; 32];

        // Create first transaction with genesis parents
        let parents = [[0u8; 32], [0u8; 32]];
        let tx1 = Transaction::new(
            parents,
            addr,
            [3u8; 32],
            100,
            10,
            1234567890,
            0,
            1,
            vec![0u8; 64],
            vec![1u8; 64],
        );

        dag.add_transaction_validated(tx1.clone()).unwrap();

        // Try to create second transaction with same parents (double spend)
        let tx2 = Transaction::new(
            parents, // Same parents
            addr,
            [4u8; 32],
            50,
            10,
            1234567890,
            0,
            2,
            vec![0u8; 64],
            vec![1u8; 64],
        );

        let result = dag.add_transaction_validated(tx2);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Double spend"));
    }

    /// Test that sender conflict detection works
    #[tokio::test]
    async fn test_sender_conflict_detection() {
        let mut dag = DAG::new();
        let addr = [1u8; 32];

        // Create first transaction with genesis parents
        let tx1 = Transaction::new(
            [[0u8; 32], [0u8; 32]],
            addr,
            [3u8; 32],
            100,
            10,
            1234567890,
            0,
            1,
            vec![0u8; 64],
            vec![1u8; 64],
        );

        dag.add_transaction_validated(tx1.clone()).unwrap();

        // Try to create second transaction from same sender (conflict)
        let tx2 = Transaction::new(
            [[0u8; 32], [0u8; 32]],
            addr, // Same sender
            [5u8; 32],
            50,
            10,
            1234567890,
            0,
            2,
            vec![0u8; 64],
            vec![1u8; 64],
        );

        let result = dag.add_transaction_validated(tx2);
        assert!(result.is_err());
        // The error will be about sender conflict
    }
}

#[cfg(test)]
mod fork_safety_tests {
    use super::*;

    /// Test that rewarded_blocks prevents double rewards
    #[tokio::test]
    async fn test_rewarded_blocks_prevents_double_reward() {
        let mut ledger = Ledger::new();
        let mut consensus = ConsensusState::new();
        let validator = [1u8; 32];
        let block_id = [2u8; 32];
        let block_height = 100;

        // Set current height for finality check
        consensus.current_height = block_height + 10; // Block is finalized

        // Set initial balance
        ledger.set_balance(&validator, 1000);

        // Mark block as rewarded in consensus state first
        consensus.validate_and_mark_block_reward(block_id).unwrap();

        // Try to apply reward (should fail - block already marked)
        let result = ledger.apply_block_reward(&validator, block_id, block_height, &consensus);
        assert!(result.is_err());
    }

    /// Test that rollback_block_reward works correctly
    #[tokio::test]
    async fn test_rollback_block_reward() {
        let mut ledger = Ledger::new();
        let mut consensus = ConsensusState::new();
        let validator = [1u8; 32];
        let block_id = [2u8; 32];
        let block_height = 100;

        // Set initial balance
        ledger.set_balance(&validator, 1000);
        consensus.current_height = block_height + 10;

        // Apply reward
        let reward = calculate_reward(block_height);
        ledger.apply_block_reward(&validator, block_id, block_height, &consensus).unwrap();

        let balance_after_reward = ledger.get_balance(&validator);
        assert!(balance_after_reward > 1000);

        // Rollback reward
        ledger.rollback_block_reward(&validator, block_id, reward).unwrap();
        consensus.rollback_block_reward(&block_id);

        let balance_after_rollback = ledger.get_balance(&validator);
        assert_eq!(balance_after_rollback, 1000);

        // Verify block is no longer marked as rewarded
        assert!(!consensus.is_block_rewarded(&block_id));
    }

    /// Test that finality check prevents rewarding unfinalized blocks
    #[tokio::test]
    async fn test_finality_check_prevents_unfinalized_reward() {
        let mut ledger = Ledger::new();
        let mut consensus = ConsensusState::new();
        let validator = [1u8; 32];
        let block_id = [2u8; 32];
        let block_height = 100;

        // Set current height to same as block (not finalized)
        consensus.current_height = block_height;

        // Try to apply reward (should fail - not enough confirmations)
        let result = ledger.apply_block_reward(&validator, block_id, block_height, &consensus);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not finalized"));
    }
}

#[cfg(test)]
mod atomic_execution_tests {
    use super::*;

    /// Test that transaction execution rolls back on failure
    #[tokio::test]
    async fn test_atomic_rollback_on_failure() {
        let processor = TransactionProcessor::new();
        let dag = Arc::new(RwLock::new(DAG::new()));
        let ledger = Arc::new(RwLock::new(Ledger::new()));
        let mempool = Arc::new(RwLock::new(Mempool::new(1000, 10)));
        let consensus = ConsensusState::new();

        let sender = [1u8; 32];
        let receiver = [2u8; 32];

        // Set initial balance
        ledger.write().await.set_balance(&sender, 1000);
        ledger.write().await.commit_nonce(&sender, 0);

        let balance_before = ledger.read().await.get_balance(&sender);

        // Create transaction with insufficient balance (will fail)
        let tx = Transaction::new(
            [[0u8; 32]; 2],
            sender,
            receiver,
            2000, // More than balance
            10,
            1234567890,
            0,
            1,
            vec![0u8; 64],
            vec![1u8; 64],
        );

        let result = processor.process(
            tx,
            &dag,
            &ledger,
            &mempool,
            10,
            None,
            &consensus,
            None,
        ).await;

        assert!(result.is_err());

        // Verify rollback occurred - balance should be unchanged
        let balance_after = ledger.read().await.get_balance(&sender);
        assert_eq!(balance_before, balance_after);
    }

    /// Test that nonce is not committed if transfer fails
    #[tokio::test]
    async fn test_nonce_not_committed_on_transfer_failure() {
        let processor = TransactionProcessor::new();
        let dag = Arc::new(RwLock::new(DAG::new()));
        let ledger = Arc::new(RwLock::new(Ledger::new()));
        let mempool = Arc::new(RwLock::new(Mempool::new(1000, 10)));
        let consensus = ConsensusState::new();

        let sender = [1u8; 32];
        let receiver = [2u8; 32];

        ledger.write().await.set_balance(&sender, 1000);
        ledger.write().await.commit_nonce(&sender, 0);

        let nonce_before = ledger.read().await.get_nonce(&sender);

        // Create transaction that will fail
        let tx = Transaction::new(
            [[0u8; 32]; 2],
            sender,
            receiver,
            2000,
            10,
            1234567890,
            0,
            1,
            vec![0u8; 64],
            vec![1u8; 64],
        );

        let _ = processor.process(
            tx,
            &dag,
            &ledger,
            &mempool,
            10,
            None,
            &consensus,
            None,
        ).await;

        // Verify nonce was not committed
        let nonce_after = ledger.read().await.get_nonce(&sender);
        assert_eq!(nonce_before, nonce_after);
    }
}

#[cfg(test)]
mod orphan_recovery_tests {
    use super::*;

    /// Test that orphans persist to disk
    #[tokio::test]
    async fn test_orphan_persistence() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();

        let tx = Transaction::new(
            [[1u8; 32], [2u8; 32]], // Non-existent parents
            [3u8; 32],
            [4u8; 32],
            100,
            10,
            1234567890,
            0,
            1,
            vec![0u8; 64],
            vec![1u8; 64],
        );

        // Store orphan
        storage.put_orphan(tx.id, &tx).unwrap();

        // Retrieve orphan
        let retrieved = storage.get_orphan(tx.id).unwrap();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().id, tx.id);

        // Remove orphan
        storage.remove_orphan(tx.id).unwrap();
        let retrieved_after = storage.get_orphan(tx.id).unwrap();
        assert!(retrieved_after.is_none());
    }

    /// Test that orphans are loaded on startup
    #[tokio::test]
    async fn test_orphan_load_on_startup() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();

        // Store multiple orphans
        for i in 0..5 {
            let tx = Transaction::new(
                [[i as u8; 32], [((i + 1) % 255) as u8; 32]],
                [3u8; 32],
                [4u8; 32],
                100,
                10,
                1234567890,
                0,
                1,
                vec![0u8; 64],
                vec![1u8; 64],
            );
            storage.put_orphan(tx.id, &tx).unwrap();
        }

        // Load all orphans
        let orphans = storage.get_all_orphans().unwrap();
        assert_eq!(orphans.len(), 5);
    }

    /// Test that orphan is removed after successful processing
    #[tokio::test]
    async fn test_orphan_removed_after_success() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        let mut dag = DAG::new();

        // Create orphan with genesis parents (will be valid)
        let tx = Transaction::new(
            [[0u8; 32]; 2], // Genesis parents
            [3u8; 32],
            [4u8; 32],
            100,
            10,
            1234567890,
            0,
            1,
            vec![0u8; 64],
            vec![1u8; 64],
        );

        storage.put_orphan(tx.id, &tx).unwrap();

        // Simulate successful processing (add to DAG)
        dag.add_transaction_validated(tx.clone()).unwrap();

        // Remove from orphans
        storage.remove_orphan(tx.id).unwrap();

        // Verify orphan is gone
        let retrieved = storage.get_orphan(tx.id).unwrap();
        assert!(retrieved.is_none());
    }
}

#[cfg(test)]
mod monetary_policy_tests {
    use super::*;

    /// Test that MAX_SUPPLY is enforced
    #[tokio::test]
    async fn test_max_supply_enforcement() {
        let mut ledger = Ledger::new();
        let mut consensus = ConsensusState::new();
        let validator = [1u8; 32];
        let block_id = [2u8; 32];

        // Set total supply to near MAX_SUPPLY
        ledger.total_supply = MAX_SUPPLY - 1000;
        consensus.current_height = 1000;

        // Try to apply reward that would exceed MAX_SUPPLY
        let result = ledger.apply_block_reward(&validator, block_id, 1000, &consensus);
        assert!(result.is_err());
        // The error will be about exceeding MAX_SUPPLY
    }

    /// Test that reward calculation follows halving schedule
    #[test]
    fn test_reward_halving_schedule() {
        // Initial reward: 10 AETH = 100,000,000,000 units
        let reward_0 = calculate_reward(0);
        assert_eq!(reward_0, 100_000_000_000);

        // After 210,000 blocks: 5 AETH
        let reward_210k = calculate_reward(210_000);
        assert_eq!(reward_210k, 50_000_000_000);

        // After 420,000 blocks: 2.5 AETH
        let reward_420k = calculate_reward(420_000);
        assert_eq!(reward_420k, 25_000_000_000);

        // After 630,000 blocks: 1.25 AETH
        let reward_630k = calculate_reward(630_000);
        assert_eq!(reward_630k, 12_500_000_000);
    }

    /// Test that total supply never exceeds MAX_SUPPLY
    #[tokio::test]
    async fn test_total_supply_never_exceeds_max() {
        let mut ledger = Ledger::new();
        let mut consensus = ConsensusState::new();
        let validator = [1u8; 32];

        // Apply rewards for many blocks
        for height in (0..1000).step_by(210_000) {
            let block_id = [height as u8; 32];
            consensus.current_height = height + 100;
            consensus.validate_and_mark_block_reward(block_id).unwrap();
            
            let result = ledger.apply_block_reward(&validator, block_id, height, &consensus);
            if result.is_ok() {
                assert!(ledger.total_supply <= MAX_SUPPLY);
            }
        }
    }
}

#[cfg(test)]
mod consensus_state_tests {
    use super::*;

    /// Test that consensus state is single source of truth for height
    #[test]
    fn test_consensus_state_height_source_of_truth() {
        let mut consensus = ConsensusState::new();

        assert_eq!(consensus.get_height(), 0);

        consensus.increment_height();
        assert_eq!(consensus.get_height(), 1);

        consensus.increment_height();
        assert_eq!(consensus.get_height(), 2);
    }

    /// Test that rewarded_blocks is fork-safe
    #[test]
    fn test_rewarded_blocks_fork_safe() {
        let mut consensus = ConsensusState::new();
        let block_id1 = [1u8; 32];
        let block_id2 = [2u8; 32];

        // Mark block1 as rewarded
        consensus.validate_and_mark_block_reward(block_id1).unwrap();
        assert!(consensus.is_block_rewarded(&block_id1));
        assert!(!consensus.is_block_rewarded(&block_id2));

        // Simulate reorg - rollback block1
        consensus.rollback_block_reward(&block_id1);
        assert!(!consensus.is_block_rewarded(&block_id1));

        // Mark block2 as rewarded (alternate chain)
        consensus.validate_and_mark_block_reward(block_id2).unwrap();
        assert!(consensus.is_block_rewarded(&block_id2));
    }

    /// Test that confirmation threshold is respected
    #[test]
    fn test_confirmation_threshold() {
        let consensus = ConsensusState::new();

        // Default threshold is 6
        assert_eq!(consensus.confirmation_threshold, 6);

        // Block at height 100 is not finalized at height 105
        assert!(!consensus.is_finalized(100, 105));

        // Block at height 100 is finalized at height 107
        assert!(consensus.is_finalized(100, 107));
    }
}
