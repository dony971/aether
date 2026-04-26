//! # AETHER SEDC - Self-Evolving DAG Consensus - Unified Implementation
//!
//! This crate implements the complete AETHER SEDC protocol including:
//! - Blockless DAG architecture (no sequential blocks)
//! - Heavy Subgraph Consensus (leaderless consensus)
//! - Adaptive probabilistic finality
//! - Dynamic reputation system
//! - Consensus-linked economics
//! - Zero-trust transaction processing
//! - Persistent orphan recovery
//! - Fork-safe reward distribution

pub mod transaction;
pub mod parent_selection;
pub mod pow;
pub mod consensus;
pub mod economics;
pub mod security_audit;
pub mod rpc;
pub mod explorer_api;
pub mod p2p;
pub mod wallet;
pub mod storage;
pub mod json_storage;
pub mod genesis;
pub mod ledger;
pub mod validation;
pub mod transaction_processor;
pub mod reputation; // NEW: Dynamic reputation system

#[cfg(test)]
mod tests;

pub use transaction::{Transaction, TransactionId, Address};
pub use parent_selection::{ParentSelectionAlgorithm, TipSet, DAG};
pub use pow::{MicroPoW, DifficultyAdjuster};
pub use consensus::{VQVConsensus, Validator, Vote, ConsensusError, ConsensusState};
pub use economics::{EmissionCurve, RewardCalculator, TokenBalance, EconomicsError, HARD_CAP, TokenAmount};
pub use security_audit::{SecurityAuditor, SecurityAudit, DoubleSpendAttempt, ParasiteChain, ValidatorSlash, SlashReason};
pub use rpc::{AetherRpcImpl, RpcError, start_rpc_server, BalanceResponse, TransactionResponse, DagStatsResponse, Mempool, DagGraphResponse, DagNode as RpcDagNode, DagEdge as RpcDagEdge, TipsResponse, MiningStatusResponse};
pub use explorer_api::{ExplorerApi, DagGraph, DagNode, DagEdge};
pub use p2p::{P2PConfig, P2PNetwork, P2PMessage};
pub use wallet::Wallet;
pub use storage::{Storage, StorageError, BatchOperation, TreeName};
pub use genesis::{GenesisConfig, GenesisBlock, GENESIS_MESSAGE, GENESIS_HASH, initialize_genesis, genesis_hash};
pub use ledger::Ledger;
pub use validation::{TransactionValidator, ValidationError};
pub use transaction_processor::{TransactionProcessor, ProcessingError};
pub use reputation::{Reputation, ReputationStore, ReputationConfig};

/// Events for the save worker MPSC channel
#[derive(Debug, Clone)]
pub enum SyncEvent {
    SaveRequested,
}
